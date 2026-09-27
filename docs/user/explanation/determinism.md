# Determinism and replay

A useful reproduction keeps the causes of an execution fixed. For Harmony, that means more than remembering a random seed: the application image, kernel, runtime, command, memory size, input data, and recorded actions all matter.

## What Harmony controls

Harmony runs a single-vCPU Linux guest and supplies controlled timing and seeded entropy. The guest's apparent time is part of the execution, rather than an unrestricted view of the host wall clock. A fault campaign records the actions and durations it applied so replay can execute those choices directly.

The deterministic execution environment is called **Consonance**. The campaign engine that explores alternatives is called **Dissonance**. You normally use both through the `harmony` CLI; there is no need to manage them as separate services.

## Why a seed is not enough

A different image can execute different instructions even with the same command and seed. A moved registry tag can silently change the image. A rebuilt kernel can change the environment. A checker that contacts an outside service introduces input outside the controlled guest.

Retain exact image bytes and runtime artifacts. Reports include hashes to help detect changed inputs, but a hash cannot reconstruct a missing file. A bug action file also needs its original runtime, image, seed, and configuration.

## What a matching replay establishes

Matching observable output is useful evidence for the particular execution you repeated. A matching machine-state digest is stronger evidence about a stopped state, but it is still tied to the execution contract and inputs under which it was obtained.

Neither result proves that every workload runs deterministically, that different host architectures produce identical states, or that the application has no other bugs. The [supported-system and workload limits](../reference/compatibility.md) bound what you can conclude.

## Host time is a separate limit

Host wall-time budgets keep an experiment from running indefinitely. If the watchdog interrupts a slow or stuck guest, that cutoff is a resource-control result, not evidence that the application violated an invariant. A wall-time-limited search may stop at a different amount of logical work on a faster host.

For repeatable comparisons, retain logical work bounds and inspect cutoff counters alongside your correctness evidence. Use [replay](../how-to/replay.md) to execute the stored input rather than hoping a new campaign rediscovers it.
