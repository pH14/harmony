# duplicate-request

A checks a missing idempotency key, B checks the same key, and both apply it. Four ordered steps create two applications. Each ledger word is both the idempotency record and the count of the request’s committed applications.

One request arrives every 2 ms, counted from setup, and both handlers receive every request. A handler that starts or restarts joins at the newest arrival. Each handler sleeps 1 ms after every request, so a handler that falls behind, for example after a pause, catches up at half speed. Both handlers therefore return to the same requests after process faults, and new unapplied keys keep arriving.

## Triple

| part | here |
|---|---|
| workload | two handler processes over one MAP_SHARED request ledger |
| fault surface | a process switch at getpid between lookup and insertion, or an EventPark in that interval |
| oracle | the Always assertion `each request is applied at most once` |

## Correct variant and oracle

Insert and apply with one compare-and-swap from missing to applied. Killing or restarting a handler cannot split the record from its side effect.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `duplicate_request.correct=1` | select the correct variant; the default is buggy |
| `duplicate_request.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case duplicate-request
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
