# aba-reuse

A reads the head and successor, B pops the head, B pops its successor, B pushes the original head, then A performs its stale compare-and-swap. Five ordered steps resurrect the node B still owns. Round coordination prevents reset from racing an unfinished operation.

This is a held-out evaluation case. Build it and check both variants, but never use its results to tune the searcher.

## Triple

| part | here |
|---|---|
| workload | one process with a stack popper and a recycler over three permanently allocated nodes |
| fault surface | EventPark after reading the head and successor and before compare-and-swap |
| oracle | the Always assertion `the stack never resurrects an owned node` |

## Correct variant and oracle

Include a monotonically increasing tag in the head word and advance it on every push and pop. Node indexes replace raw pointers and all concurrent node links use atomics, so the case has no use-after-free or undefined C data race. The oracle runs after both operations finish and rejects a successful pop whose successor is B’s retained node.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `aba_reuse.correct=1` | select the correct variant; the default is buggy |
| `aba_reuse.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case aba-reuse
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
