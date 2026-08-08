# Dissonance from scratch — a fuzzing-vocabulary design sketch

**Status: exploration note, not a ruling.** This document describes a
from-scratch redesign of the dissonance search loop. It deliberately uses
standard fuzzing vocabulary instead of harmony's own names (for example *run*
where the glossary says *rollout*, *corpus* where the code says `Archive`).
Nothing here retires a `docs/GLOSSARY.md` ruling; if any of this is adopted,
it goes through the normal glossary process.

The implementation plan that goes with this document is `docs/LIBAFL-PLAN.md`.
The code lives in `dissonance-v2/`.

## The idea in one paragraph

We want to find bugs and interesting states in a deterministic system by
searching its state space. Mechanical search is good at trying millions of
things quickly. LLMs are good at judging what matters and at writing code.
This design keeps the two strictly apart: a standard fuzzing loop does all
the exploring at machine speed, and LLMs sit outside that loop, reading its
output files and occasionally installing new code that changes what the loop
looks for. No LLM call ever happens inside a run.

## Why the split works

An LLM call takes about a second. A fuzzing decision takes microseconds.
That is six orders of magnitude, so an LLM must never sit on the critical
path of a run. Two facts make the combination work anyway:

1. **The target cannot tell the LLM is slow.** The system under test is
   deterministic and can be paused and resumed at any point. LLM slowness
   costs wall-clock time only, and wall-clock time can be bought back with
   parallel calls to cheap models.
2. **One LLM output steers many runs.** The LLM never picks a single action.
   It produces things that are reused across thousands of runs: labels on
   corpus entries, priorities, and — most importantly — code.

The pattern to remember: **LLMs judge individual cases until they can write
down a rule, then the rule replaces them.** A cheap model labels corpus
entries one at a time. A bigger model reads those labels, spots the pattern,
and writes code that makes the same judgment mechanically from then on.

## The five components

1. **Executor.** Runs the target: `run(input) → observations`. In production
   this is the deterministic VM; during development it is a maze toy or a
   game emulator. Thousands of runs per second. Never contains an LLM.
2. **Corpus.** The set of runs worth extending, stored on disk. A run's
   result joins the corpus only if it is *novel* — it reached a part of the
   coverage/feature map that nothing else has reached.
3. **Scheduler.** Picks which corpus entry to extend next and how much
   effort to spend on it. Favors entries whose recent extensions produced
   novelty. Purely mechanical.
4. **Triage.** A pool of cheap LLMs running *behind* the search. As new
   entries appear, a triage model reads each one's logs and evidence and
   attaches labels: a priority hint, tags, a one-line summary, hypotheses.
   The fuzzing loop never waits for triage.
5. **Instrumentor.** A bigger LLM, invoked occasionally. It reads the
   fuzzer's stats and the labeled corpus — the same things a human fuzzing
   operator would read — and emits **code**: new novelty detectors and new
   mutators (details below). Its output installs between runs, never during
   one.

One sentence of glue: the fast loop looks wherever the coverage map is
detailed, and the LLMs' only power is to change where the map is detailed.

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
              generated detectors                       ▼
              and mutators,     │                 Triage — cheap LLMs,
              priority caps     │                 label entries, stream
                                │                       │ labels + stats
                                └── Instrumentor ◄──────┘
                                    (LLM, occasional)

        SLOW LOOP (LLM speed, async — fast loop never waits)
```

## A worked example: the locked door

Suppose the target is an NES game. An input is a sequence of button presses.
The base coverage map is coarse: it distinguishes states by (room number,
player position). The fuzzing loop mutates button sequences from corpus
entries and keeps any run that lands somewhere new on the map. Because kept
entries are extended rather than rediscovered, near-random button mashing
makes steady progress — it is a random walk with a ratchet.

Now the search plateaus. There is a locked door in room 7 and a key in room
3. Runs reach the door constantly. Some run eventually grabs the key too —
but the map only tracks (room, position), so "at the door holding the key"
lands in the same map cell as "at the door empty-handed". Occupied cell means
not novel, so **the run that grabbed the key is thrown away**. The search can
execute the solution; the feedback just cannot see it.

Triage notices: "forty entries at the door; the inventory byte is zero in
every one of them." The instrumentor reads those labels and emits a detector
— a few lines of code that expose (room, has-key) as a new feature. After a
rebuild and restart (the corpus survives on disk), the next run that picks up
the key is *kept*, the scheduler extends from it, and random mutation opens
the door within minutes.

Note what the LLM never did: it never chose a button, never said "go get the
key". It made a distinction visible; the mechanical ratchet did the rest.

## What the LLMs produce

Three kinds of output, all of them files, none of them live decisions:

- **Labels** (triage). Attached per corpus entry. A small machine-readable
  part (priority hint, duplicate-of, flags) feeds the scheduler; free-text
  tags and hypotheses feed the instrumentor.
- **Generated detectors** (instrumentor). Code implementing one pure
  function: observations in, feature keys out. Each detector adds a new map
  for the novelty check. Detectors change **what the search can see**.
- **Generated mutators** (instrumentor). Code implementing one pure
  function: input sequence in, mutated sequence out. These install
  *semantic macros* — coherent multi-action patterns like "partition the
  leader while a write is in flight" or "jump-arc of length N" — that plain
  single-action mutation would only compose by luck. Mutators change
  **what the search can do**.

Rules that keep this safe:

- **Scope by lineage.** A generated detector can be restricted to runs that
  descend from a named corpus entry. A bad detector then wastes effort in
  one subtree instead of steering the whole campaign into a ditch.
- **Add, never modify.** Generated detectors add new maps; the base map is
  never edited. Regions of the search stay comparable.
- **Mechanical retirement.** A generated detector that stops producing
  novelties, or a generated mutator whose offspring stop producing
  novelties, is dropped automatically. No LLM is involved in retirement.
- **Machine-consumed label fields are enums.** A cheap model cannot break
  the scheduler with free text.

## Designing an input vocabulary

Every target needs a small algebra of typed actions; an input is a sequence
of them. For a game: button chords with hold durations. For a distributed
KV store: client operations (`Put(k,v)`, `Get(k)`, `Cas(k,old,new)`) and
fault operations (`Partition{a,b}`, `Kill(node)`, `DropNext{link,n}`,
`Heal`) interleaved in one sequence — a fault is just an action with a
position in time.

Guidelines, learned from fuzzing and property-based testing:

- **Pick the altitude of the target's public interface.** Too low (raw
  frames, raw syscalls) and mutations are valid but meaningless. Too high
  (canned scenarios like "do a failover") and the search can only shuffle
  stories someone already wrote. Use the interface's primitive verbs plus
  primitive faults; let mutation and macros build the structure.
- **Every action must be total.** After mutation changes the start of a
  sequence, the rest must still mean something. Reference stable structural
  names (node ids, links, "the next N packets on a→b"), never ephemeral
  identities from a particular run ("packet #4133"). An action that cannot
  apply degrades to a no-op; the run continues.
- **Keep parameter domains small.** Keys from a handful of values, not from
  `u64`. Collisions and interference are where bugs live.
- **Mutate at several granularities.** Parameter-level (change a key),
  action-level (swap a `Get` for a `Cas`), sequence-level (splice, delete a
  span), timing-level (move a partition relative to the writes). That is a
  stack of four mutators, not four architectures.

## Vocabulary

| this design          | replaces (current harmony)        | where the term is from |
|----------------------|-----------------------------------|------------------------|
| run                  | rollout                           | fuzzing (execs/sec)    |
| corpus / corpus entry| `Archive` / `Entry`               | AFL                    |
| coverage/feature map | cells (`IdentityCells`)           | AFL / MAP-Elites       |
| novelty check        | admission                         | greybox fuzzing        |
| scheduler / energy   | bandit / selection weights        | AFL / AFLFast          |
| triage               | assess loop (Resolution, partly)  | fuzzing (crash triage) |
| instrumentation      | codify loop (Resolution, partly)  | IJON, laf-intel        |
| snapshot             | MomentRef                         | snapshot fuzzing       |
| campaign             | campaign                          | already standard       |

The old rollout/step/campaign/Resolution ladder collapses to **run →
campaign**; the loops in between need no nouns ("between runs" covers it).

## How to test it without LLMs

LLMs never act directly; they emit data and code through two narrow seams
(evidence → labels, stats + labels → generated code). Consequences:

- The fast loop tests as an ordinary fuzzer: determinism properties, novelty
  checks, scheduler behavior, scoping, retirement.
- The seams test with scripted stand-ins: a regex triager, a hand-written
  detector. What is being verified is the plumbing, not the model.
- **Replay is the master property.** Record every label and every generated
  file as it arrives. A campaign is then a pure function of (seed, recorded
  artifacts) and replays end-to-end with no LLM present. That is both the
  integration test and the reproduction story.
- Model quality is measured separately, as A/B campaigns on benchmark
  targets with known deep states (time-to-discovery versus a null triager).
  Never in CI.

## How to validate before fault injection

The executor interface is generic over input vocabularies, so the search can
be validated on targets that need no fault injection at all:

1. Toy state machines / maze programs with known deep states — exact
   time-to-state metrics.
2. An NES/SDL game via key presses — tests the LLM-as-game-player thesis
   directly.
3. An SDK-instrumented program where the input is the sequence of decisions
   the SDK requests.

Fault injection arrives later as one more input vocabulary, not as an
architectural event.

## Open questions

- Whether interactive LLM trajectory seeding (an LLM plays once, slowly, at
  the macro-action level; the recorded sequence joins the corpus; mutation
  riffs on it) earns its plumbing over having the LLM write the sequence
  blind. Severable either way — a seeded trajectory is just a corpus entry.
- Relationship to the existing `dissonance/` crates if this design is
  adopted: port ideas, or converge vocabularies.
