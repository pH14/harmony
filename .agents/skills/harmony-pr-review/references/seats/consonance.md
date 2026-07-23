# Seat: Consonance — is record==replay still bit-identical?

Your assignment: determinism, the property this project exists to deliver. A silent leak
here poisons every downstream result and surfaces later as an unattributable hash
divergence — far more expensive to localize than a review citation.

Mandated procedure:

1. Trace every new input to hashed/recorded state: is it a fault-input on the one shared
   hashed stream, or observation-only (hash-neutral)? Anything ambiguous is a finding —
   the classification itself is part of the contract.
2. Save/restore/seal discipline: does restore reject every state that save could never
   produce? Is a failed restore atomic, or does it leave the object half-mutated? Does
   round-trip bit-identity hold — including with armed/pending/staged state in flight
   (restore-while-armed is an observed bug class here)?
3. Reseed/fold/compose interactions: does folded == sequential hold, bit-identical, for
   any path touching entropy draws, schedules, or markers? If the PR asserts stream
   independence, look for the pinned equivalence test.
4. Mechanical sweep, then follow every hit to its consequence: `HashMap`/`HashSet`
   iteration reaching output, hashes, or encoded bytes; floating point in state-affecting
   code; wall-clock time; unseeded randomness; PID/env values leaking into recorded bytes.
5. Moment/stamping: events stamped with the Moment they occur, not with a loop anchor or
   batch boundary (mis-stamping is an observed post-merge escape class).

P1 for this lens: any nondeterminism reachable in recorded or replayed state; any
hash-affecting change without its version bump and golden regeneration; any save/restore
asymmetry.
