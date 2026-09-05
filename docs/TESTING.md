# Testing determinism

Harmony uses three acceptance oracles. They distinguish replay identity from
specification conformance and controlled variation.

## O1: identity

O1 runs the same subject twice with the same seed and compares complete state at
regular work checkpoints and at termination. It compares `state_hash` and run
termination rather than final output alone.

When a checkpoint differs, `unison` re-executes the subject and bisects the
matching and mismatching interval to identify the first divergent work count. A
bounded run is reported as identical only through its tested limit.

## O2: conformance

O2 runs the subject and compares its final `observable_digest` with a reviewed
golden. It detects repeatable but incorrect behavior in values such as CPU
identity, time, entropy, protocol responses, and workload results.

Goldens cover guest-emitted evidence rather than every latent byte of machine
state. O1 covers full-state identity.

## O3: seed sensitivity

O3 runs a declared workload under two different seeds and examines its
guest-observable output.

- An entropy-consuming, control-flow-stable workload performs the same amount
  of work and produces different observable output.
- A seed-pure workload produces the same observable output.

O3 uses `observable_digest` rather than `state_hash`. The full state contains
the seeded entropy stream and can differ even when the guest does not use it.
Both runs must reach a terminal state within the test limit. Otherwise the
result is inconclusive.

## Hashes and localization

`state_hash` is a canonical digest of modeled architectural and latent state,
including memory, CPU, devices, time, timers, entropy position, and guest-service
state. It is used for replay identity and divergence localization.

`observable_digest` covers bytes the workload exposes, such as its report,
serial, or event stream. It is used for specification goldens and
seed-sensitivity checks.

Component digests can localize a mismatch to RAM, registers, a device, V-time,
or a guest channel. They are diagnostic breakdowns rather than separate
definitions of identity.

## Corpus

The acceptance manifest registers each test as a cell with a workload, corpus
kind, oracle set, eligible host class, and virtualization level. A cell that
cannot run on the selected host is unrun rather than passed.

The corpus has three families:

- Microprograms isolate instruction, register, timer, interrupt, and guest
  protocol behavior. They can test boundary cases and exact values.
- Generated cases and fuzz seeds exercise decoders, snapshot operations, model
  state machines, and instruction or input combinations. Pure-logic targets run
  broadly; virtualization-dependent cases run on the required backend.
- Real workloads test the composed VM, controlled guest environment, services,
  snapshots, and application behavior together.

Microprograms provide localization. Real workloads provide composition
evidence.

## Contracts and fixpoints

Shared contract tests apply the same behavioral exam to each implementation of
a replaceable boundary.

Backend obligations fall into three categories:

- ordering: operations and completions occur in the sequence the engine
  expects;
- exactness: reported values, dirty-page sets, deadlines, and capabilities have
  their stated meaning;
- fixpoint: saving, restoring, and saving again returns the same canonical
  state.

Snapshot codecs reject missing, duplicate, out-of-order, incompatible, and
malformed records. Portable snapshot tests corrupt each load-bearing section
and verify that import fails before a handle is created.

CPU qualification exercises the exposed instruction and register surface,
identity policy, save and restore fixpoints, and normalized behavior on each
host composition. A result qualifies that composition rather than every CPU or
backend of the same architecture.

## Event and schedule checks

Final-state identity does not establish that time and interrupts were handled
correctly. V-time tests compare the normalized event sequence, post-event clock,
checkpoint hashes, immutable deadline schedule, and event boundary on which
each interrupt was raised.

An independent placement checker recomputes the first eligible boundary from
the schedule instead of consuming the run loop's result. Backend-private exits
remain available for local diagnosis and are not compared as portable events.

Campaign replay performs a corresponding check at the search layer. It
re-executes recorded jobs, recomputes result digests and frame counts, reapplies
archive admission, and compares the resulting decisions with the campaign
stream.

## Anti-vacuity checks

A test result is meaningful only if the claimed failure can affect it. The test
suite uses these checks:

- empty manifests, empty oracle lists, and zero-checkpoint identity runs fail;
- missing hardware prerequisites produce an unrun or inconclusive result;
- bounded-prefix results report where verification stopped;
- seed-sensitivity requires distinct seeds and terminal executions;
- comparators are exercised against deliberately corrupted state, schedules,
  artifacts, or expected values;
- parsers reject unknown fields, so a misspelled declaration cannot become an
  empty configuration;
- mutation, property, fuzz, and proof checks exercise invariants beyond example
  tests.

Each gate identifies the behavior it establishes, covers the production path
where that behavior matters, and includes a representative failing case.

Development commands and CI configuration live in contributor guidance and
automation. Component-specific fixtures and format details live beside their
owning code.
