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
ceiling comes from the evaluator's full-replay contract. Famidash uses bounded
expansion: the budget charges edge attempts and one unit for an empty task;
executed edges, failed preparation, tasks and replay work are distinct counters.
The v2 expansion stream retains observation counts and fresh SHA-256 commitments
to their postcard encoding. Full replay regenerates the observations and checks
those commitments alongside every tree decision and the final report/checkpoint.
Cold boot, final
serialization and verification are included in campaign seconds; search
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
counters. For bounded expansion it starts after all workers are ready and ends
after stream finish, integrity verification, final archive/report and snapshot
checkpoint construction, immediately before the search returns. It includes
dispatch and finalization, including final checkpoint persistence, and excludes
earlier boot and later evaluator witness verification. The linear runner's
counter also includes live-observer construction, encoder startup and worker
readiness. These counters include work outside individual coordinator spans.
Native `dispatch_phase` instead starts after all
workers are ready, before reservation, and stops before final checkpoint/report
persistence. Use `coordinator_host` for the coordination capacity bound; retain
`dispatch_phase.host` as a narrower diagnostic. Measure encoder CPU and
end-to-end persistence separately, without adding dispatch or finish to the
whole coordinator counter.
Do not add nested or concurrent timers to infer elapsed time.

For an expansion coordinator diagnostic, set `HARMONY_COORDINATOR_PROFILE=1`.
`telemetry.coordinator` then contains inclusive wall-time spans for reservation,
selection, root preparation, completion, validation, evidence merging, archive
insertion, maintenance, observation, record construction, output and checkpoints.
These spans locate expensive operations; nested spans and scheduling waits make
their sum unsuitable as coordinator CPU. Use `coordinator_host` for the whole
CPU cost, and repeat capacity trials with this optional clock instrumentation off.
The same profile setting also measures each worker's actual execute closure with
Linux thread CPU time, including required restore, reconstruction, snapshot
capture and observation work. It excludes queue waits, reply delivery and
post-job target telemetry. Its `execute_cpu` coverage reports calls, samples,
ordinary errors, panics and overflow; unsupported or incomplete coverage cannot
supply a worker service cost. The boundary includes clock and unwind-probe
overhead and is not a guest-instruction-only floor. The qualifier reports
`worker_execute_cpu_ns_per_task` only for complete, successful job coverage with
the matching admitted-task denominator. Repeat with profiling off to retain the
observer effect, and compare full logical artifacts within the same executable.
Profiling state is local to the session and is not checkpointed or recorded in
the logical stream.

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

The qualifier overrides `workers`, `window` and `verification`. Normal Famidash
uses an **absolute outstanding-task window**: it sends `window: 512` unchanged
at every worker count, including counts that do not divide 512. Each task can
attempt up to 14 edges, and its ordered admission determines subsequent roots.
The other current NES routes use reservations per worker; for those routes the
qualifier sends 512/worker count and requires exact divisibility. No algorithm
flag selects this behavior. Changing the logical window changes adaptive
search and invalidates a fixed-work speedup comparison. The native Famidash
launcher also uses the absolute task window.

On a quiet host, select physical CPUs of one core type (leave room for the
coordinator, two stream serializers, ordered relay, feed and browser/tunnel). For example, on a machine
with at least 133 usable CPUs:

```sh
taskset -c 0-132 python3 scripts/qualify-nes-scaling.py \
  --evaluator workloads/nes/target/release/nes-eval \
  --request /srv/runs/famidash-scaling-request.json \
  --workers 1 2 4 8 16 32 64 128 --window 512 --repeats 3 \
  --out /srv/runs/famidash-scaling-001
```

For lab checks use `--workers 1 2 4 8`. The qualifier refuses more workers than
the process affinity permits unless `--allow-oversubscription` is explicit;
oversubscribed runs are functional evidence only. Affinity is held fixed across
trials but does not force the one-worker arm onto each individual core. For
precise efficiency estimates also measure each selected core separately, as
[Performance](../../docs/PERFORMANCE.md) requires. Record host load, core types,
thermal/frequency behavior and cgroup capacity; `/proc` inventories and load
averages are retained, but do not prove an idle host. Capacity measurements
must include the whole campaign's cgroup memory rather than process RSS alone.

`summary.json` reports per-count median/range, speedup, total campaign time,
worker busy fractions and explicitly labelled throughput. For Famidash,
throughput is admitted tasks per search second, and coordinator CPU, encoder
CPU and producer wait are reported **per task**, not per executed edge. The
summary retains executed-edge rates, attempts, consumed budget, failed
preparation, empty tasks and bootstrap/reconstruction/edge frame work separately.
It verifies each result counter against the campaign report and derives session
work by checked final-minus-initial subtraction. Worker `jobs` must equal
session admitted tasks, including checkpoint-pending tasks executed again;
newly reserved tasks alone are not that denominator. The qualifier itself
requires fresh requests; the counter validator also understands resumed reports.
Linear routes continue to use jobs as their execution and cost unit.
Stream costs distinguish `stream_aux_cpu_ns_per_task`, the sum of all stream
thread CPU for a shared auxiliary-core budget, from the two serializer costs
and relay cost. `stream_dedicated_core_bottleneck_ns_per_task` is their maximum,
valid only when all three have measured CPU coverage and each has a separate
core. It is unavailable when a role is missing or counters overflow. It is a
necessary service bound, not a measured throughput or placement guarantee.
The final coordinator stream-drain CPU is reported separately when profiling
supplies it. It is already included in expansion's whole `coordinator_host`
counter; add it only when using the narrower dispatch counter. Include all
other auxiliary work in the model. The qualifier reports this whole counter as
`coordinator_whole_cpu_ns_per_task`, with scheduler wait separately; unsupported
coverage remains null. `coordinator_cpu_ns_per_task` retains the narrower dispatch
cost, and neither it nor stream finish is added to the whole value.
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
```

These are resource ceilings offered by the launcher, not a claim that 64 GiB
fits every long search. Choose them from the actual host capacity and keep
headroom for per-worker emulators and observation buffers. Run
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

The tests plant a valid-but-different trial stream, corrupt stream identity,
missing replays, incomplete telemetry and a failed evaluator. Expansion fixtures
use the current evaluator's cumulative and session fields; planted wrong
engines, counter underflow, bad frame totals and edge-based worker counts must
fail. The fixed-window test includes three workers with a window of 512. These
checks preserve the run evidence on failure.
