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
experiment or proof of saturation on an unmeasured host. The 10,000 execution
ceiling comes from the evaluator's full-replay contract. Cold boot, final
serialization and verification are included in campaign seconds; search
seconds and worker busy fractions are reported separately. Busy time includes
restore, execution and capture; it does not measure guest cycles. Read raw
`telemetry.json` for coordinator occupancy, admission stalls and completed
buffer peaks. Do not add nested or concurrent timers to infer elapsed time.

## Prepare the host

Use Linux, the repository toolchain, a C++ compiler and the pinned native
QuickNES core and Famidash ROM. Keep assets, builds and evidence outside the
repository, and retain the source revision, lockfiles and build log beside the
executable. The existing `benchmarks/search/eval.py build` provides source-bound
build records; a manual build needs equivalent provenance.

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

The qualifier overrides `workers`, `window` and `verification`. In evaluator
requests `window` means **reservations per worker**, so it writes 512/worker
count for a fixed window of 512. Every worker count must divide that window.
Changing the absolute window changes the adaptive search and invalidates a
fixed-work speedup comparison. A launcher run uses four reservations per worker;
at 128 workers that is the same absolute window of 512.

On a quiet host, select physical CPUs of one core type (leave room for the
coordinator, feed and browser/tunnel). For example, on a machine with at least
129 usable CPUs:

```sh
taskset -c 0-128 python3 scripts/qualify-nes-scaling.py \
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

`summary.json` reports per-count median/range, speedup, total campaign time and
worker busy fractions. No numerical speedup threshold is treated as a passing
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
missing replays, incomplete telemetry and a failed evaluator. They verify that
those cases fail while preserving the run evidence.
