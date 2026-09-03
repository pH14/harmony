# Exploration

dissonance searches a deterministic machine for useful states. It chooses
which captured state to extend and which controlled inputs to try, then judges
the observations returned by the target. It is not part of the machine's
transition function.

## Terms

- A `Moment` is a point on a machine's deterministic progress axis.
- A timeline is one ordered execution history.
- A rollout restores or branches from captured state, applies an input
  continuation, and runs to an endpoint or terminal condition.
- A snapshot is an implementation-owned handle to captured state. It supports
  fast branching but is not durable evidence on its own.
- A reproducer is the recorded input or environment that reconstructs a
  timeline. Its bytes are owned and interpreted by the machine.
- A campaign is a seeded, budgeted sequence of rollouts against one workload
  under one recorded set of search policies.

A control session is the lifetime of a connection to a machine. One campaign
may use several machine instances or sessions.

## Campaign loop

A campaign starts from one of three origins:

- genesis, with the workload's empty input;
- a snapshot root supplied by an evaluator;
- a prior archive whose retained tree becomes the new population.

For each job, the coordinator:

1. selects a retained parent;
2. derives a mutation seed from the campaign's worker stream;
3. expands that seed into an action suffix under the recorded mutation policy;
4. restores the parent's snapshot and executes the suffix;
5. gathers observations and candidate endpoints;
6. applies the recorded admission and replacement rules;
7. records the job and its decisions in reservation order.

The workload adapter owns action meaning, snapshot representation, state
decoding, progress milestones, terminal conditions, and input-generation
policy. The campaign engine owns budgeting, selection, admission order,
persistence, and replay.

## Branching and replay

The machine boundary distinguishes two operations that start with a restore:

- `branch` restores a snapshot and installs a new recorded environment for
  exploration;
- `replay` restores a snapshot verbatim for reproduction.

consonance exposes this distinction directly. Emulator-backed targets provide
the same behavior through deterministic state restore and recorded actions.
dissonance can use either target shape without changing the archive algorithm.

Snapshots are an acceleration structure. The recorded input remains the
authoritative path. A missing snapshot can be rebuilt by replaying actions from
a known ancestor or genesis. Snapshot checkpoint files make a large resumed
campaign faster, but are validated against the target and campaign origin.

## Search archive

dissonance uses a bounded quality-diversity archive. A workload maps an observed
state to an ordered archive key. The key supplies several grouping depths:

- the finest group is a retention slot in which nearby candidates compete;
- coarser groups pool related states for parent selection;
- workload-owned preference compares states within the same location;
- route cost and insertion identity provide deterministic tie-breaking.

The structure retains several kinds of progress instead of a single global
best path. Parent selection walks the recorded groups and samples selectable
entries. An optional retirement policy can reduce selection of barren entries
or groups without removing the history required for replay.

Archive size and logical memory can be bounded. Under pressure, selectable
snapshots and other acceleration data are evicted according to deterministic
policy. The append-only campaign stream remains sufficient to reconstruct the
logical result.

## Mutation and observations

An action is defined by the target. An action that is not applicable in the
current state is a no-op, so the generic searcher does not need target rules.
Mutation policies derive finite suffixes from recorded seeds. They can sample a
fixed vocabulary, bias an empirical draw table, or splice a tail from retained
paths. Policy identities and derived table checkpoints are recorded.

Observations are evidence rather than moves. A target can expose decoded
mechanical state, changed memory locations, guest events, logs, coverage
fingerprints, or an exit classification. The adapter turns those observations
into archive keys and milestones. Reading them does not change the machine.

For a consonance-backed target, dissonance supplies opaque workload actions
through the guest environment and consumes guest-published state and events.
The generic coordinator does not need CPU registers, physical addresses, or
guest-specific protocol details.

## Parallel execution

Workers execute jobs in parallel. The coordinator owns decisions that can
affect campaign state:

- each logical worker has a seed derived from the campaign seed and worker
  index;
- selection and mutation occur in a deterministic reservation sequence;
- completed jobs are admitted in reservation order;
- host completion order changes waiting time, not the archive or recorded
  stream.

Without a wall-time cutoff, the same campaign configuration, origin, workload
bytes, and seed produce the same stream under the current scheduler. A
wall-time cutoff can stop issuing work and determine the prefix completed by a
live run. It does not change the recorded meaning of completed jobs.

## Replay records

Exploration produces two layers of replay evidence:

1. The machine reproducer records the environment needed to replay one timeline
   or failure.
2. The campaign stream records the search configuration and each selected
   parent, mutation seed, result identity, and admission decision.

Campaign replay runs the recorded jobs serially. It recomputes results and
checks their digests, frame counts, admission decisions, policy versions, and
origin identities. A mismatch is a replay failure.

Campaign summaries and progress curves derive from the stream. Host throughput
and operator-facing progress timing are diagnostics and do not rank machine
states.

## Current targets

The repository exercises dissonance with deterministic NES targets. The direct
QuickNES path provides emulator snapshots. The consonance-backed Nova path runs
QuickNES inside a controlled Linux VM and maps input prefixes to whole-VM
snapshots local to each evaluator thread. Both present the same game-level
action and observation model to the campaign engine.

A new workload still needs an action vocabulary, observations, progress keys,
and terminal predicates. The current workload adapters do not define a general
search policy for arbitrary software.
