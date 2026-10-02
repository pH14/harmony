# missed-wakeup

The consumer checks an empty queue and releases the mutex, the producer enqueues and signals, then the consumer waits without rechecking. Three ordered steps lose the signal. A watchdog observes the stalled consumer after a guest-time grace period.

## Triple

| part | here |
|---|---|
| workload | one process with producer, consumer and watchdog threads over a condition-variable queue |
| fault surface | EventPark after an empty check and before the unconditional wait, or a park on another thread that holds the mutex while the consumer waits to relock it |
| oracle | the Always assertion `a queued item never loses its wakeup` |

## Correct variant and oracle

Keep the mutex across the predicate check and wait in a loop. The oracle requires a queued item for at least 15 seconds of monotonic guest time, a consumer still inside its wait, and a wait that started after the last signal. The signal sequence excludes a correctly signaled but delayed consumer even across repeated Pause windows. Killing the single node discards the whole queue and all three threads together.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `missed_wakeup.correct=1` | select the correct variant; the default is buggy |
| `missed_wakeup.noise=N` | add N distinct instrumented function calls per iteration, in producer work and in the buggy check-to-wait gap; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. A found execution also needs 15 s of guest time after the lost
signal for the watchdog to report it. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case missed-wakeup
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
