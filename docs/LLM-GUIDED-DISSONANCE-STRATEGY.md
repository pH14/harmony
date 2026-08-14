# LLM-guided dissonance: strategy

## Executive thesis

Harmony should not use an LLM as a low-level fuzzing policy. It should use an LLM as the
semantic strategist above a high-throughput deterministic search engine.

The division of labor is:

- **consonance** provides exact execution, snapshot/branch, replay, and counterfactual control;
- **dissonance Progression** performs cheap mechanical exploration over large numbers of
  rollouts;
- **resolution** uses an LLM, or a human through one, to invent better search abstractions and
  experiments between sealed batches of execution.

The LLM does the work for which pretrained semantic knowledge matters: recognizing meaningful
states, proposing temporally extended tactics, deriving assertions and progress signals, and
forming causal hypotheses. The mechanical engine does the work for which throughput and exactness
matter: expanding those proposals across timings and parameters, retaining useful states, and
replaying discoveries.

This is a hybrid of quality-diversity search, Go-Explore, program synthesis, and active causal
testing. It is not a replacement of search or RL with token-by-token model inference.

## System shape

Harmony already has the right three-level decomposition:

| Loop | Unit | Cadence | Policy |
|---|---|---:|---|
| Modulation | decision or perturbation | microseconds | seeded, open-loop tactic |
| Progression | rollout | milliseconds to seconds | archive, mutation, selector |
| Resolution | campaign or sealed batch | minutes to hours | LLM/human judgment |

The core invariant should remain: each loop is fixed while it executes and may be revised only by
the loop above. In particular, there should be no LLM callback in the per-decision or per-rollout
hot path.

For somewhat faster adaptation, resolution may operate over **sealed micro-epochs**: run a fixed
batch of rollouts, finalize its artifacts, ask the agent to produce a new strategy artifact, then
start another fixed batch. This preserves the artifact-shaped boundary and never makes worker
progress depend on model latency.

## The LLM's output is a program, not an action

The main integration primitive should be a typed, versioned, deterministic strategy artifact. A
conceptual proposal is:

```rust
struct SearchProposal {
    base: ArchiveQuery,
    goal: StatePredicate,
    tactic: TacticProgram,
    mutations: ParameterSpace,
    stop: StopPredicate,
    budget: u64,
}
```

One proposal should expand mechanically into hundreds or thousands of rollouts. The model chooses
the semantic shape; dissonance explores the combinatorics.

Candidate products of resolution include:

1. **Tactics and regimes.** Parameterized macro-actions or fault programs, such as partitioning a
   leader near lease expiry and healing after a competing election. Progression mutates exact
   timings, values, and combinations.
2. **Waypoint predicates and cell functions.** Executable definitions of meaningful intermediate
   states. Examples include `(leader, term, commit-index bucket, durable-log length)` for a
   replicated service or `(room, inventory, health bucket)` for a game.
3. **Oracles and assertions.** Checks derived from specifications, source, logs, or recurring
   behavioral patterns. These turn latent failures into terminal findings.
4. **Instrumentation proposals.** Requests for new SDK state registers, trace fields, matchers, or
   assertions in the next guest build.
5. **Counterfactual experiments.** Exact branches that vary one fault or answer at a chosen
   `Moment` to distinguish competing explanations.
6. **Reusable skills.** Successful tactic fragments stored as parameterized programs and composed
   into later proposals.

All outputs must compile into an allowlisted DSL or existing Harmony configuration surface. Free
text can explain a proposal but must never be executable authority.

## Why deterministic execution changes the opportunity

LLM-guided game agents normally struggle with long horizons, approximate state recovery, and the
cost of repeated low-level interaction. Consonance removes the state-recovery problem: a promising
state can be restored exactly, and a successful trajectory is a permanent, replayable artifact.
This complements Go-Explore's “return, then explore” principle directly.

More importantly, deterministic branching makes **active causal testing** practical. After a
finding, resolution can form hypotheses such as:

- the violation requires a partition overlapping lease expiration;
- a failed durable write is causal rather than merely correlated;
- the failure becomes inevitable before the externally visible crash;
- a particular hidden state variable would be a useful progress descriptor.

Each hypothesis can be compiled into a batch of same-prefix, one-change counterfactuals. The LLM
interprets the results and proposes the next experiment. This moves the system beyond discovery
toward automated explanation, search refinement, and families of related bugs.

## Games as an evaluation instrument

Games are useful because search progress is deep, sparse, and visually inspectable. They should be
treated as an exploration benchmark, not as the target product.

For Super Mario Bros. or Metroid, resolution can consume representative frontier frames, structured
RAM-derived registers, and successful and failed input tapes. It can then propose waypoint
predicates and temporally extended input programs. Progression restores frontier states and mutates
the programs' timing and chord choices at high throughput. Successful fragments become skills.

Two evaluation tracks are required:

- **knowledge-allowed:** the model may use source, RAM maps, manuals, and domain knowledge. This
  resembles real software testing, where knowledge of TCP, Raft, PostgreSQL, and common failure
  modes is useful prior information;
- **blind generalization:** use an unfamiliar or procedurally modified workload and withhold
  semantic documentation. This separates planning and search improvement from memorized game
  knowledge.

## Determinism, trust, and cost

Model inference itself need not be reproducible. Its output must be immutable and recorded. Every
strategy artifact should include or reference:

- the exact agent input dossier and output;
- model and prompt identifiers;
- DSL/compiler version;
- artifact hash;
- deterministic rollout-budget allocation.

A search batch is then reproducible from its campaign seed and strategy artifact. A found bug
continues to replay solely from its genesis-complete `Environment`; reproducing it never invokes a
model.

Floating-point embeddings or live model scores should not enter state-affecting campaign logic.
Resolution may use them to reason, but it should emit explicit predicates, cell IDs, integer
priorities, or tactic programs for deterministic execution.

The model is an untrusted source of proposals. Each proposal receives a bounded budget and is
promoted only by measured yield. Ordinary novelty search and pure-random exploration remain
permanent control arms so that model bias cannot collapse the search space. Guest output must also
be treated as hostile data: structured telemetry is delimited from instructions, and the agent may
act only through typed Harmony verbs.

Cost is controlled by batching and amortization. Model calls occur on a plateau or epoch boundary,
produce many candidate programs, and run asynchronously from execution workers. Repeated successful
patterns should be cached, compiled into fixed templates, or eventually distilled into a smaller
local policy.

## Evaluation plan

The first question is not whether an LLM can produce impressive trajectories. It is whether the
hybrid finds more useful behavior per fixed execution budget.

Run equal-branch-budget comparisons between:

1. pure random exploration;
2. the existing archive search;
3. archive search plus randomly generated tactics using the same DSL;
4. archive search plus LLM-generated tactics;
5. optionally, archive search plus human-authored tactics.

Report depth, distinct held-out cells, discovery curves, branches per second, model cost, and
proposal yield. For bug workloads, the primary measure is bug discovery, not coverage alone. Keep
the game used for strategy development separate from the held-out game used for the generalization
claim.

If the game result is positive, transfer the same mechanism to planted distributed-system bugs.
Replace frames and input chords with run traces, state registers, fault regimes, and invariant
code. Measure time or rollouts to bug, reproducibility, minimization quality, and whether proposed
descriptors and oracles transfer across programs.

## Staged strategy

1. **Artifact-only advisor.** Give resolution frozen campaign artifacts and let it propose CellFn
   candidates, oracles, and tactic programs. A human ratifies every artifact.
2. **Automated batch proposals.** Compile validated proposals and allocate a capped minority of the
   rollout budget to them while retaining mechanical and random controls.
3. **Skill library and micro-epochs.** Retain successful parameterized tactics and allow resolution
   to revise the next sealed batch after plateaus or discoveries.
4. **Causal investigation loop.** Let the agent drive exact counterfactual batches from
   `MomentRef`s and promote confirmed distinctions into new instrumentation.
5. **Distillation.** Move recurring, measurable behaviors from expensive frontier-model calls into
   deterministic templates or cheap local policies.

The strategic bet is therefore narrow but substantial: LLMs should supply semantic priors and
programs that multiply deterministic search, while consonance and dissonance retain responsibility
for exhaustive execution, measurement, and proof. If successful, Harmony's differentiator is not
that an LLM can play a game or guess an input; it is that an agent can repeatedly turn semantic
hypotheses into exact, massive, reproducible experiments.

## Selected references

- Ecoffet et al., [“First return, then explore”](https://www.nature.com/articles/s41586-020-03157-9),
  *Nature*, 2021.
- Wang et al., [“Voyager: An Open-Ended Embodied Agent with Large Language
  Models”](https://arxiv.org/abs/2305.16291), 2023.
- Ma et al., [“Eureka: Human-Level Reward Design via Coding Large Language
  Models”](https://arxiv.org/abs/2310.12931), 2023.
- Meng et al., [“Large Language Model Guided Protocol
  Fuzzing”](https://www.ndss-symposium.org/ndss-paper/large-language-model-guided-protocol-fuzzing/),
  NDSS 2024.
- Xia et al., [“Fuzz4All: Universal Fuzzing with Large Language
  Models”](https://arxiv.org/abs/2308.04748), ICSE 2024.
