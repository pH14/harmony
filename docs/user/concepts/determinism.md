# Determinism and investigation

Harmony saves the starting state, program and runtime identities, and recorded
inputs needed to reconstruct an execution. This lets you return to a finding
without repeating the search that discovered it.

The seed alone wouldn’t be enough: search decisions also depend on the state
accumulated during exploration. A saved finding therefore includes the recorded
path as well as the identities of the executables and runtime used to follow it.

## Investigate from a saved point

When you open a branch, Harmony reconstructs the selected point so you can
continue from it. Commands and interactive shells can change files, processes,
and application state, and the saved branch retains those changes for further
searches.

Even a diagnostic command can advance the live guest or affect its behavior.
Keep the original finding for comparison as you investigate on a branch.

## Keep the original artifacts

Reproducing a finding requires the matching executable and runtime artifacts.
Harmony checks their saved identities and rejects mismatches. If you rebuild
the program with a fix, testing that new binary is a separate experiment; the
old result cannot establish how the fixed version will behave.

Host time limits also affect how much work completes. A shorter time budget can
stop a run before it reaches the same point, and a host timeout by itself is
not a failed guest assertion.

The repository’s [determinism argument](https://github.com/pH14/harmony/blob/main/docs/DETERMINISM.md)
sets out the full contract and how it is validated.
