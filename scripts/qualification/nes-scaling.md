# One NES campaign across many cores

`scripts/qualify-nes-scaling.py` compares one retained native `nes-eval`
executable at several worker counts. It holds the seed, memory budget,
execution budget and **absolute admission window** fixed, runs alternating
ascending/descending worker orders, and retains every request, log, full
decision stream, checkpoint, logical report and worker/coordinator telemetry.
Every trial must complete full campaign replay and two witness replays. Its
entire stream, checkpoint bytes and logical report must match the one-worker
reference. A changed executable, wall-limited run or mismatch fails the check
and preserves evidence. A new build needs a separate output directory and its
own one-worker reference.

This is a bounded throughput and ordering check, not a whole-game effectiveness
experiment or proof of saturation on an unmeasured host. The 10,000 execution-budget
ceiling comes from the evaluator's full-replay contract. Every current NES
route uses the same campaign job accounting and per-worker reservation window.
Cold boot, final serialization and verification are included in campaign seconds; search
seconds and worker busy fractions are reported separately. Busy time includes
restore, execution and capture; it does not measure guest cycles. Read raw
`telemetry.json` for coordinator occupancy, admission stalls and completed
buffer peaks. Native runs serialize on two bounded background threads and hash their output
in order on a relay thread. Each has separate CPU, scheduler-wait and queue-wait
telemetry. The borrowed output sink still runs on the coordinator; its cost
and final drain must remain in the accounting. Campaign records must serialize
deterministically from their owned values. Serialization and destruction can
run ahead; output bytes and failure prefixes retain logical order.
`coordinator_host` samples coordinator CPU and scheduler wait even with verbose
profiling off. It is absent when the operating system cannot supply these
counters. The linear runner's counter includes live-observer construction,
encoder startup, worker readiness, dispatch and finalization before the search
returns. It excludes later evaluator replay and witness verification. These
counters include work outside individual coordinator spans.
Native `dispatch_phase` instead starts after all
workers are ready, before reservation, and stops before final checkpoint/report
persistence. Use `coordinator_host` for the coordination capacity bound; retain
`dispatch_phase.host` as a narrower diagnostic. Measure encoder CPU and
end-to-end persistence separately, without adding dispatch or finish to the
whole coordinator counter.
Do not add nested or concurrent timers to infer elapsed time.

For a coordinator diagnostic, set `HARMONY_COORDINATOR_PROFILE=1`.
`telemetry.coordinator` then contains admission, bookkeeping, history compaction,
stream write and selection wall durations. A restored prefix can make these
fields incomplete. Nested work and scheduling waits make their sum unsuitable
as coordinator CPU. Use `coordinator_host` for whole CPU cost, and repeat
capacity trials with optional verbose instrumentation off.
The independent `HARMONY_WORKER_CPU_PROFILE` presence switch measures each
worker's actual execute closure with Linux thread CPU time, including required
restore, snapshot capture and observation work. It excludes queue waits, reply delivery and
post-job target telemetry. Its `execute_cpu` coverage reports calls, samples,
ordinary errors, panics and overflow; unsupported or incomplete coverage cannot
supply a worker service cost. The boundary includes clock and unwind-probe
overhead and is not a guest-instruction-only floor. The qualifier reports
`worker_execute_cpu_ns_per_job` only for complete, successful job coverage with
the matching completed-job denominator. For capacity, compare fresh default
arms with both switches unset against worker-only arms with only `HARMONY_WORKER_CPU_PROFILE` present. Require own full
replay and witnesses and equal logical artifacts in the same executable. Use
whole C and S0/S1/H maxima over both arms, and only complete worker-only CPU for
the worker service cost; never substitute zero for default or unsupported worker CPU. Set both switches
only for a separately labeled full diagnostic. Presence enables either switch,
even with a value of `0`. Worker `profile_mode` is explicit host-only provenance:
`off`, `coordinator_only`, `worker_only` or `coordinator_and_worker`. The qualifier
records requested switch presence, requires actual consistent mode agreement,
and reports both enabled roles. Missing, unknown, mixed or contradictory modes
fail validation; no old full-profile measurement silently becomes a worker-only
capacity arm. Retain earlier full-profile failures in their original scope.
Measurement counters are session-local; switch selection is cached for the
process lifetime and cannot be toggled within a process. Switches are neither
checkpointed nor recorded in the logical stream.

## Prepare the host

Use Linux, the repository toolchain, a C++ compiler and the pinned native
QuickNES core and Famidash ROM. Keep assets, builds and evidence outside the
repository, and retain the source revision, lockfiles and build log beside the
executable. The existing `benchmarks/search/eval.py build` provides source-bound
build records; a manual build needs equivalent provenance.
The NES workspace's release profile uses portable ThinLTO and one codegen unit;
the Arcade launcher uses this same profile. Retain any environment or Rust flag
overrides with the build record, since they can change the executable and its
performance.

```sh
scripts/build-quicknes-core.sh /srv/assets/quicknes_libretro.so
workloads/nes/scripts/build-famidash-rom.sh
cargo build --locked --release --manifest-path workloads/nes/Cargo.toml --bin nes-eval
```

Create `/srv/runs/famidash-scaling-request.json` using the real absolute asset
paths and their SHA-256 values:

```json
{
  "game": "famidash", "whole_game": true,
  "rom": "/srv/assets/famidash.nes",
  "core": "/srv/assets/quicknes_libretro.so",
  "rom_sha256": "REPLACE_WITH_ROM_SHA256",
  "core_sha256": "REPLACE_WITH_CORE_SHA256",
  "seed": 27, "workers": 1, "executions": 8000,
  "memory_mib": 8192, "window": 512, "wall_seconds": 3600,
  "mixture": "alphabet_only", "verification": "campaign"
}
```

The qualifier overrides `workers`, `window` and `verification`. Every NES route
uses reservations per worker: the qualifier divides the chosen absolute window
by the worker count and requires exact divisibility. With `--window 512`, the
one-worker request has `window: 512`, and the 128-worker request has `window: 4`.
There is no game-specific engine or window selection. Changing the absolute
logical window changes adaptive search and invalidates a fixed-work speedup
comparison. The Arcade launcher uses a per-worker window of four for every game;
its total window therefore grows with the worker count. The qualifier's fixed
window comparison must not be presented as identical to every launcher setting.

On a quiet host, select physical CPUs of one core type. In addition to the
workers, budget CPU for one coordinator, two stream serializers and one ordered
relay. A dedicated-role service model therefore needs 132 cores for 128 workers
before live observation and host services. As an illustrative headless budget,
use 134 usable CPUs, allowing two more for monitoring and host work:

```sh
taskset -c 0-133 python3 scripts/qualify-nes-scaling.py \
  --evaluator workloads/nes/target/release/nes-eval \
  --request /srv/runs/famidash-scaling-request.json \
  --workers 1 2 4 8 16 32 64 128 --window 512 --repeats 3 \
  --out /srv/runs/famidash-scaling-001
```

For lab checks use `--workers 1 2 4 8`. The qualifier refuses more workers than
the process affinity permits unless `--allow-oversubscription` is explicit;
this check counts workers only and does not reserve the auxiliary cores above.
Oversubscribed runs are functional evidence only. Affinity is held fixed across
trials and shared by the campaign threads; it does not dedicate a core to each
role or force the one-worker arm onto each individual core. A model that shares
stream roles on one core must use their summed CPU cost. For
precise efficiency estimates also measure each selected core separately, as
[Performance](../../docs/PERFORMANCE.md) requires. Record host load, core types,
thermal/frequency behavior and cgroup capacity; `/proc` inventories and load
averages are retained, but do not prove an idle host. Capacity measurements
must include the whole campaign's cgroup memory rather than process RSS alone.

`summary.json` reports per-count median/range, speedup, total campaign time,
worker busy fractions and explicitly labelled job throughput. Result executions
must match the campaign report's completed jobs and the sum of worker `jobs`.
Costs use completed jobs as their denominator; a missing or contradictory
counter fails validation. Every trial starts a fresh campaign.
Stream costs distinguish `stream_aux_cpu_ns_per_job`, the sum of all stream
thread CPU for a shared auxiliary-core budget, from the two serializer costs
and relay cost. `stream_dedicated_core_bottleneck_ns_per_job` is their maximum,
valid only when all three have measured CPU coverage and each has a separate
core. It is unavailable when a role is missing or counters overflow. It is a
necessary service bound, not a measured throughput or placement guarantee.
Whole coordinator CPU includes final stream drain and report/checkpoint work.
The qualifier reports it as `coordinator_whole_cpu_ns_per_job`, with scheduler
wait separately; unsupported coverage remains null.
`coordinator_cpu_ns_per_job` retains the narrower dispatch cost. Do not add the
nested dispatch counter or final drain to the whole value. Include all other
auxiliary work in the model.
Missing CPU coverage remains null rather than a zero cost. These costs do not
automatically establish a worker-count ceiling; apply the capacity model in
[Performance](../../docs/PERFORMANCE.md) using equivalent-core worker measurements.
No numerical speedup threshold is treated as a passing
result. Compare repeated same-host baseline and candidate arms as separate
builds; both must establish their own exact replay oracle. Hold this bounded
job fixed, then run a longer campaign to inspect archive growth, snapshot
residency, re-execution and sustained contention before spending the cloud
machine's full budget.

## Watch and record the large run

Build the Arcade site as documented in
[`demos/twin-dragons`](../../demos/twin-dragons/README.md), and add the cloud
machine to `~/.config/harmony/arcade-machines.toml`:

```toml
[machines.cloud]
ssh = "cloud-search"
dir = "/srv/harmony-arcade"
workers = 128
memory_mib = 65536
memory_max_mib = 81920
```

These are resource ceilings offered by the launcher, not a claim that 64 GiB
fits every long search. Choose them from the actual host capacity and keep
headroom for per-worker emulators and observation buffers. For a live 128-worker
run, an illustrative CPU budget is 136 physical cores: 128 workers, four
coordinator/stream roles, one live-feed allowance and three for OS, monitoring
and network work. This budget does not enforce thread placement. The browser's
rendering and video encoding use the viewing computer's resources.

The archive budget is only part of whole-campaign RAM. Also allow for worker
targets, pending job origins and results, stream buffers, live observation and
checkpoint/export transients. Required machine `memory_max_mib` is the explicit
whole-unit ceiling, greater than the archive ceiling `memory_mib`; it is not an
automatic overhead estimate. The example sets 80 GiB per run, not an aggregate
machine reservation. Budget simultaneous runs plus host services within host
RAM. The launcher sets this `MemoryMax` and `MemorySwapMax=0`, without a CPU set.
Retain `memory.max`, `memory.swap.max`, `memory.peak` and memory
events when qualifying a machine. Measure a longer run at the intended archive
budget before allocating the remaining host RAM to the search.

Run
`demos/twin-dragons/tools/launcher.py` on the viewing computer, open its
loopback URL, select **Native runs**, then Famidash and the machine. The launcher
ships committed HEAD, builds it remotely, uses a systemd memory limit and
forwards the live page. Its discovery can reattach after a launcher restart.
Use the page's recording control to capture the displayed camera and minimap;
keep the browser active while recording. The search runs on the cloud machine
and the video encoder runs on the viewing computer. The page README documents
recording format, limits and download behavior.

## Check the qualifier

```sh
python3 -m unittest discover -s scripts -p test_qualify_nes_scaling.py
```

The tests plant a valid-but-different trial stream and binary checkpoint, corrupt
stream identity, missing replays, inconsistent completed-job counters, incomplete
telemetry and a failed evaluator. They check independent profiling provenance,
unsupported CPU coverage, every stream role and counter overflow. Fixed absolute
windows and worker divisibility apply equally to Famidash and the other NES
games. A mismatch preserves the actual requests, logs and artifacts.
