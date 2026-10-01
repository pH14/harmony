# mini-wal-reset

The checkpointer reads a header, the writer resets the log and appends fewer entries, then the checkpointer copies using the old count and generation. Three ordered protocol steps mix log generations. Alternating full and half-length logs supplies the reset-to-shorter-log transition.

## Triple

| part | here |
|---|---|
| workload | one process with a log writer and checkpointer over a fixed in-memory WAL |
| fault surface | EventPark between reading the log header and completing the checkpoint copy |
| oracle | the Always assertion `a checkpoint contains one complete log generation` |

## Correct variant and oracle

Hold a read lock across the header and entire copy; the writer holds the matching write lock across reset and append. Every entry encodes its generation and index. The oracle compares every copied entry with the captured header, so a mixed copy or stale tail cannot pass. Atomics keep the buggy copy defined; there is no disk or durability fault. Killing the node resets log and checkpointer together.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `mini_wal_reset.correct=1` | select the correct variant; the default is buggy |
| `mini_wal_reset.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case mini-wal-reset
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
