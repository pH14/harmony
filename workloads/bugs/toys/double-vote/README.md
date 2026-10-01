# double-vote

Candidate A checks that the voter has not voted, candidate B checks the same state, and both record a vote and become leaders. Four ordered protocol steps elect two candidates in the same term. Each barrier-delimited round represents a fresh term.

## Triple

| part | here |
|---|---|
| workload | one process with two candidate request threads sharing a voter |
| fault surface | EventPark between checking voted-for and storing the selected candidate |
| oracle | the Always assertion `one term never elects two leaders` |

## Correct variant and oracle

Hold a mutex around the check and set of voted-for. Each accepted vote elects its candidate: in the three-member cluster, the candidate already has its own vote and the voter supplies the second vote needed for a majority. The oracle checks the accumulated leader bitmap only after both request threads finish the term, before clearing it for the next round. Killing the node resets the whole term.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `double_vote.correct=1` | select the correct variant; the default is buggy |
| `double_vote.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case double-vote
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
