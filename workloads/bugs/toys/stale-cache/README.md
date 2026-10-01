# stale-cache

The reader misses the cache and captures the old store version, the writer updates the store and invalidates the cache, then the reader fills it with the old value. Three ordered protocol steps create a stale cache entry.

This is a held-out evaluation case. Build it and check both variants, but never use its results to tune the searcher.

## Triple

| part | here |
|---|---|
| workload | one process with a cache reader and store writer |
| fault surface | EventPark between reading the backing store on a cache miss and filling the cache |
| oracle | the Always assertion `a filled cache never predates its invalidation` |

## Correct variant and oracle

Check the captured version while holding the store/cache mutex before filling; discard an obsolete fill. A store version is also its value, so equality with the current version is a complete cache-fill oracle. The update and invalidation are one protected operation; the single planted mistake is admitting an obsolete fill. Killing the node resets all components together.

## Knobs and expected difficulty

| kernel knob | effect |
|---|---|
| `stale_cache.correct=1` | select the correct variant; the default is buggy |
| `stale_cache.noise=N` | add N distinct instrumented function calls per iteration, outside the sensitive operations; clamp to 0 through 32 |

The default noise is zero. Expected search difficulty is hundreds to thousands
of executions. Noise changes the competing event sites; its measured effect
belongs in campaign results, not this specification. Every loop sleeps for 1 ms
to let the guest's single vCPU switch runnable threads at a system call.

## Build and evaluate

From the repository root, use `workloads/bugs/interleaving.py --case stale-cache
--build` with the CLI, kernel and initramfs paths described in the collection
README. The image contains a static instrumented executable on `scratch` and
fits a 256 MiB guest. The composed `libvoidstar.so` is included at its standard
path to satisfy event admission; the executable retains the complete static
runtime archive. Compilation uses `-Wall -Wextra -Werror` and is separate from
linking. The runner runs controls before buggy variants and confirms case
assertions through replay. All run records stay under `target/`.
