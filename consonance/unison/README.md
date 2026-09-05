# unison

`unison` is the determinism harness and divergence bisector. It compares two
fresh machines created with the same seed, detects differing architectural
state, and re-executes from scratch to locate the first divergent work count.
The `Subject` abstraction treats work as an opaque monotonic counter: VM exits
for a real adapter and retired instructions for the bundled toy machine.

## Harness API

Implement `Subject` with `run_to`, `work`, `state_hash`, and
`observable_digest`, and implement `SubjectFactory::spawn` to create a fresh
machine for a seed. `state_hash` covers all architectural state. The separate
`observable_digest` covers only guest-visible output, which lets callers test
seed-sensitive output without confusing latent PRNG/device state for an
observable difference.

`compare_runs` hashes both machines at a caller-selected interval until they
halt or reach a limit. It reports identical runs, a checkpoint bracket, or a
halt-count mismatch, and records whether the limit—not termination—ended the
comparison. `bisect_divergence` verifies the bracket endpoints and binary
searches fresh executions in `O(log(hi - lo))` probes. Its first-divergence
claim assumes the observed difference remains persistent within the bracket.

## Reference machines

`ToyMachine` is a tiny deterministic register VM with eight registers, 64 KiB
of memory, a program counter, output log, seeded xorshift64* state, and one work
unit per retired instruction. Its instruction set and state-hash layout are
stable and fully exercised by the harness tests. `ToyFactory` and
`generate_program` create repeatable programs for bounded comparisons.

`FlakyFactory` wraps a `Perturbable` machine and applies one selected
`Perturbation` at a work boundary. It supports register/PRNG XOR and forced
halt, including perturbation at spawn and a never sentinel. This is a test
instrument for validating detection and bisection, not a production machine
adapter.

The `unison` binary runs the toy comparison and prints JSON; exit code 0 means
no difference was observed, 2 reports a detected difference, and 1 reports an
input or harness error. Unit, property, mutation, CLI, determinism, and public-
API tests cover exact counts, edge cases, and serialized reports.
