# Determinism and investigation

An execution is determined by its starting state, program and runtime artifacts,
and recorded inputs. Saving those inputs lets Harmony reconstruct the path to a
finding without repeating the whole search that discovered it.

A seed alone is not a complete description of a finding. Search decisions also
depend on accumulated search state. That is why a saved finding contains its
recorded path and execution identities.

## What a branch preserves

Harmony reconstructs the selected point, then continues from that state. A guest
command or interactive shell can change files, processes, and application state.
The saved branch retains that resulting state for further exploration.

Inspecting a live guest can itself advance or change the execution. Treat a
branch with extra diagnostics as a new experiment. The original evidence stays
available for comparison.

## What the guarantee does not cover

Rebuilding the application, changing its runtime, or moving to a different
execution identity is not exact replay of the original finding. A result from
one binary does not prove anything about the same input on a fixed binary.
Saved artifact checks reject mismatches rather than pretending they reproduce
the old execution.

Host time limits are resource bounds. A timeout is not a guest assertion, and a
shorter wall-clock budget can stop before the same logical work completes.

The repository's [determinism argument](https://github.com/pH14/harmony/blob/main/docs/DETERMINISM.md)
describes the exact contract and its validation.
