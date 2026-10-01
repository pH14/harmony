# torn-read

The reader checks an even sequence, copies the first field, the writer publishes a new pair, and the reader copies the second field. Four ordered steps expose the missing final sequence check.

## Triple

| part | here |
|---|---|
| workload | two processes sharing a sequenced two-field record |
| fault surface | EventPark between the reader’s first sequence check and its last field load |
| oracle | the Always assertion `a copied record has matching fields` |

## Correct variant and oracle

Read the sequence again after copying and accept only an unchanged even sequence. Atomics keep the planted race defined by C; they do not make a multi-field copy atomic. A killed writer leaves an odd sequence that readers ignore until its restart publishes a new generation.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `torn_read.correct=1` | select the correct variant; the default is buggy |
| `torn_read.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case torn-read
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
