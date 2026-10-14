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
Comparison and bisection compare both values. An output-only mismatch is a
divergence even when the architectural state hashes agree; `DivergencePoint`
reports both state hashes and both observable digests so that case remains
visible to callers.
`run_to(t)` must report `ReachedTarget` only at work count `t` and must not
advance past `t` while reporting `Halted`, or move the work counter backward.
Comparisons and bisection reject these inconsistencies; equal state hashes cannot certify runs sampled at
different work counts. An already-halted subject can report its existing work
count above a requested target without advancing. Comparison uses the actual
halt count to detect mismatches.
For a given seed and target, a fresh subject must produce the same outcome,
work count, state hash, and observable digest whether `run_to(target)` is called
once or after any increasing sequence of earlier targets. `spawn(seed)` must
also reproduce the same starting state. Without this segmentation-invariance
contract, a checkpoint bracket cannot identify a replayable work coordinate.
The bundled toy and perturbation adapters are qualified against direct and
segmented calls; custom adapters must establish the same property.

`compare_runs` checks both machines immediately after spawn, then compares their
state hashes and output digests at a caller-selected interval until they halt or
reach a limit. It reports identical runs, a checkpoint bracket, an equal-work
terminal divergence, or a halt-count mismatch, and records whether the
limit—not termination—ended the comparison. A zero-work mismatch is reported
even when the limit is zero.
Before returning a reached-target divergence bracket, comparison replays both
the last matching checkpoint and the mismatching checkpoint on fresh subjects.
It refuses the bracket if either direct replay has a different outcome or
observation. This detects some segmentation-sensitive adapters at the point
where they would produce an unreplayable bracket; the adapter contract remains
necessary for all intermediate bisection probes.
If both machines have already halted at the same work count, the report records
termination for any limit without comparing another checkpoint.
When both machines halt at the same work count but their terminal observations
differ, the report uses `TerminalDiverged`. This names the observed halt count
and last matching checkpoint without claiming a first divergent work count:
a fresh run to the halt count may stop at a pre-halt state, while the terminal
transition happens only when asked to run farther. The CLI reports this
divergence with exit code 2 and no bisection point. Direct bisection
also refuses a mismatching probe that halted at a different work count from
the requested target, because that terminal observation cannot be assigned
the probe's work coordinate. A terminal observation reached at the requested
work count remains bisectable.
Already-halted machines spawned at equal nonzero work also report a differing
initial observation as `TerminalDiverged` at their actual halt count; reporting
it at work zero would provide an unreplayable bisection coordinate. An initial
difference at work zero remains directly bisectable.
`bisect_divergence` checks the initial state when the lower bound is zero,
returns zero if the machines already differ there, and otherwise verifies the
bracket endpoints and probes each work count in ascending order with fresh
executions. This costs `O(hi - lo)` probe pairs but finds the first mismatch
even when observations diverge temporarily and then reconverge. The upper
endpoint must still differ for the supplied interval to be a valid bracket.

## Reference machines

`ToyMachine` is a tiny deterministic register VM with eight registers, 64 KiB
of memory, a program counter, output log, seeded xorshift64* state, and one work
unit per retired instruction. An empty program halts at work zero; falling past
the final instruction or jumping beyond the program halts at that instruction's
work boundary. Its state hash also includes the complete
instruction program, so two machines with identical registers and memory but
different future instructions differ at work zero. Its instruction set and
state-hash layout are stable and fully exercised by the harness tests.
`ToyFactory` and `generate_program` create repeatable programs for bounded
comparisons.

`FlakyFactory` wraps a `Perturbable` machine and applies one selected
`Perturbation` at a work boundary. It supports register/PRNG XOR and forced
halt, including perturbation at spawn and a never sentinel. This is a test
instrument for validating detection and bisection, not a production machine
adapter.

The `unison` binary runs the toy comparison and prints JSON; exit code 0 means
no difference was observed, 2 reports a detected difference, and 1 reports an
input or harness error. Unit, property, mutation, CLI, determinism, and public-
API tests cover exact counts, edge cases, and serialized reports.

## Host hashing

On native ARM64 Linux and macOS, SHA-256 uses runtime-detected CPU acceleration,
including when this component is built independently. Other targets and Miri
retain the existing backend selection. Hash inputs and outputs are unchanged.
The [host SHA qualification](../../scripts/qualification/README.md) checks independent builds and compares real consumers with software hashing.
