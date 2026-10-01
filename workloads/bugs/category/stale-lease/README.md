# stale-lease

A validates a lease, A is delayed past its expiry, B acquires a newer lease and writes, then A writes with its old token. Four ordered steps expose the missing store fence. Leases last 20 ms of monotonic guest time.

## Triple

| part | here |
|---|---|
| workload | one process with two lease clients and a token-checking store |
| fault surface | EventPark after validating a lease but before writing to the store |
| oracle | the Always assertion `store writes never go backwards in fencing tokens` |

## Correct variant and oracle

Reject a write below the highest fencing token the store has accepted. Lease acquisition and individual store writes are protected by separate mutexes; the planted mistake is trusting an earlier lease check across the gap between them. Killing the node resets clients and store together.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `stale_lease.correct=1` | select the correct variant; the default is buggy |
| `stale_lease.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case stale-lease
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
