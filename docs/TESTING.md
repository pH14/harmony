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
- every ignored test has a registered runner, and CI fails on one without, as
  `docs/WORKFLOWS.md` describes;
- bounded-prefix results report where verification stopped;
- seed-sensitivity requires distinct seeds and terminal executions;
- comparators are exercised against deliberately corrupted state, schedules,
  artifacts, or expected values;
- parsers reject unknown fields, so a misspelled declaration cannot become an
  empty configuration;
- mutation, property, fuzz, and proof checks exercise invariants beyond example
  tests.

Each check identifies the behavior it establishes, covers the production path
where that behavior matters, and includes a representative failing case.

## Content lints

`scripts/custom-lints.py` checks file name and fixed text patterns: workload
names outside `workloads/`, personal references, misplaced files. It cannot
judge what a file actually says. `scripts/semantic-lints.py` covers that gap
by asking TypeSafe's Jev model whether a changed file reads as a run record
or status report, carries decision residue (a rejected alternative, an old
name, a reviewer-driven change), or names one of the project's own workloads
(a specific game, database, or distributed system) in workload-agnostic
code. It also asks whether a standalone program outside `workloads/` (a
`src/bin` target or a Python or shell script) is a one-off: nothing in CI,
build configuration, or a shipped command runs it, and a person starts it by
hand to read what it prints. The model sees every line elsewhere in the
repository that names the program. Checks a probe would print belong in tests
that assert, ignored when they need hardware. The judgments need
`TYPESAFE_API_KEY`; without it, they print a skip message and pass. Semantic
violations that predate the check are recorded in
`docs/semantic-lints-baseline.json`, and that file only shrinks. A new finding
is fixed in the file. `--update-baseline` removes entries for fixed files and
exits nonzero on a new finding. `--changed-from REV` fails when the baseline
holds an entry that REV does not, with or without the key. Static checks use
`docs/custom-lints-baseline.json` where permitted; repository vocabulary and
CI contract violations cannot be baselined.

Development commands and CI configuration live in contributor guidance and
automation. Component-specific fixtures and format details live beside their
owning code.

Repository vocabulary is checked by `scripts/custom-lints.py` in every tracked
UTF-8 text file and filename, including extensionless files and the checker
itself. The prohibited term encoded by `PROHIBITED_WORD` is rejected as a word,
plural, or snake/camel-case identifier component. Larger words such as
`aggregate` and `propagate` remain valid. Vocabulary violations cannot be
suppressed through the custom-lint baseline.
