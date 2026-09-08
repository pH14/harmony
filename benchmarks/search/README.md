<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Local search evaluation

This suite evaluates Dissonance search mechanisms against fixed NES workloads.
It is designed for a private Linux host with licensed ROMs; ordinary CI uses
synthetic engine/runner tests and source-built games. A stage fixture is never
counted as a whole-game solve. No run starts from a supplied solution tape.

## Run

Build the pinned QuickNES core with `scripts/build-quicknes-core.sh`. Build Nova
and Super Tilt Bro with the scripts in `workloads/nes/scripts`. Supply your own
SMB, MM2, and Metroid ROMs. Put their absolute paths and SHA-256 values in a
private inventory outside the repository:

```json
{
  "core": {"path": "/private/assets/quicknes_libretro.so", "sha256": "YOUR_CORE_SHA256"},
  "smb": {"path": "/private/assets/smb.nes", "sha256": "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea"},
  "mm2": {"path": "/private/assets/mm2.nes", "sha256": "49136b412ff61beac6e40d0bbcd8691a39a50cd2744fdcdde3401eed53d71edf"},
  "metroid": {"path": "/private/assets/metroid.nes", "sha256": "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6"},
  "nova": {"path": "/private/assets/nova.nes", "sha256": "9107be62a08a0ae51a01f900bd18a95a52fe043f53e9cae4a64d1e8e73114b08"},
  "stb": {"path": "/private/assets/stb.nes", "sha256": "6f80d56ce0b242a4faceafafea321feb1c364ab8e7937646e8580ae9289a4ec3"}
}
```

From the repository root, with Python 3.9+, the pinned Rust toolchain, and Linux
`taskset` available:

```sh
python3 benchmarks/search/eval.py build --out /private/builds/search-001 --jobs 24
python3 benchmarks/search/eval.py run benchmarks/search/qualification.json \
  --assets /private/assets.json --binary /private/builds/search-001/nes-eval \
  --build-info /private/builds/search-001/build-info.json \
  --out /private/runs/qualification-001 --jobs 6 --cpus 24 \
  --memory-capacity-mib 40000
python3 benchmarks/search/eval.py run benchmarks/search/evaluation.json \
  --assets /private/assets.json --binary /private/builds/search-001/nes-eval \
  --build-info /private/builds/search-001/build-info.json \
  --out /private/runs/evaluation-001 --jobs 3 --cpus 24 \
  --memory-capacity-mib 40000
```

Repeat the qualified whole-game SMB gate with the same identified build:

```sh
python3 benchmarks/search/eval.py run benchmarks/search/smb-reference.json \
  --assets /private/assets.json --binary /private/builds/search-001/nes-eval \
  --build-info /private/builds/search-001/build-info.json \
  --out /private/runs/smb-reference-001 --jobs 1 --cpus 24 \
  --memory-capacity-mib 8192
```

This searches from a new game on each registered seed and fails if any cell
misses its declared victory budget. The five-seed reference passed on ms02;
its measured resource costs and the separate stress-panel failures are in
[`results`](results/README.md).

Builds and matrix output directories must be new. The build helper checks that
source identity is unchanged during compilation and records compiler versions,
source hash, flags and executable hash. `--build-info` is checked against the
binary when it includes a binary digest. An arbitrary supplied legacy build
identity is an operator attestation; absent build information is reported as
unavailable, never inferred from an unrelated current checkout.

`--case ID` restricts a run to explicitly selected registered cases. The runner
allocates disjoint CPU sets and reserves each cell's logical memory budget plus
`--overhead-mib` (default 1024) against host capacity. These are scheduling
reservations, not OS memory enforcement. Keep other heavy processes off the
host for publishable timing comparisons. CPU affinity, host model, concurrent
job limit, budgets, executable, core, ROM, adapter and mechanism identities are
retained in every matrix. Native resource sampling uses Linux `/proc`; missing
platform measurements must not be interpreted as zero.

`search.result_slots` optionally permits one (the default) or two unadmitted
result-bearing jobs per physical executor. With two slots, an executor can run
another already-reserved job while its earlier result awaits ordered admission.
This is a bounded execution experiment, independent of the logical `window`.
It can increase worker-result RSS, which is outside the logical archive budget.
The request and native identity record the choice; omission means one and keeps
older evaluation binaries usable. Compare one versus two at the same logical
window and work budget, and check stream hashes as well as memory and throughput.
The pilot and full native evaluation use a two-reservation window and two result
slots after the isolated 18-pair execution comparison reproduced every stream.
The dedicated SMB regression panel retains its original one-reservation profile
as a separate stress condition.

## Registered panels

| Manifest | Purpose |
| --- | --- |
| `qualification.json` | Six small cases: all five games plus whole-game Nova configuration. Full stream/checkpoint replay and twice-repeated witness replay; 500 executions per case. |
| `ci.json` | Source-built Nova (level and whole-game origins) and STB through the common runner, with full small-campaign replay and a frame cap. No licensed commercial ROM is used. |
| `pilot.json` | Three exploratory seeds on SMB, Nova level 1 and whole game, Metal Man, Metroid new game and STB Hard. |
| `evaluation-continuation.json` | Frozen candidate for the full panel: learned continuation replay with the original parent selector. Selected from the completed pilots before any full-panel outcome was observed. |
| `evaluation.json` | Main-mechanism control: five seeds across SMB, five Nova level fixtures plus whole-game Nova, all eight MM2 Robot Master stages, Metroid new game, and STB Easy/Fair/Hard. |
| `smb-reference.json` | Practical fresh whole-game SMB recipe: 24 workers, 2,048 MiB, count weighting, two-reservation window/two result slots, 600,000 executions and 120 million frames. Five fresh validation seeds; every cell must solve. |
| `smb-regression.json` | Fresh whole-game SMB at 24 workers and both 256/2048 MiB, five seeds. Every cell must solve within its declared budget. |
| `throughput.json` | Short isolated 24-worker runs across all five games, three seeds, a two-reservation window and 512 MiB. Copy it and change only `result_slots` from 1 to 2 to measure physical overlap. Whole-game completion is not required in this work-limited panel. |

Seeds 20260905–20260907 form the development pilot. The dedicated SMB gate adds
20260908–20260909; those seeds have now been observed in count-policy validation.
The practical SMB reference validates on 20260910–20260914. Before any broad
evaluation cell ran, its seeds were moved to the separate, preregistered panel
20260920–20260924, preserving unobserved trials for
validating a mechanism selected from the development runs. Performance panels have explicit
frame, execution and wall ceilings. SMB's dedicated regression panel keeps the
400,000-execution gate; the broad eight-worker panel allows 600,000 executions
under an 80-million-frame cap. The practical SMB reference allows 600,000
executions and 120 million frames at 24 workers. It solved all four development
seeds (including seed 1) in 122–167 seconds and all five fresh validation seeds
in 89–251 seconds, including witness verification. One validation seed needed
598,013 executions, close to the 600,000 ceiling. These are distinct resource
conditions; the stress-panel failures remain recorded.

All manifests specify exact ROM hashes and normal menu origins. MM2 is currently
an independent-stage panel; it does not claim full-game evaluation. Metroid
progress is reported even when its ending is not reached. Nova whole-game runs
require all 40 cleared-level flags from level 1; a single cleared level cannot
satisfy that predicate. Isolated later-level Nova setup is a declared fixture.

Copy a manifest to change **search** mechanisms for an ablation. Keep case IDs,
origins, seed panel, ROM/core, adapter policies and resource budgets fixed. The
comparison command rejects mismatches rather than quietly combining them.
Engine experiments are described in [SYNTHESIS.md](SYNTHESIS.md); prototype claims
are not accepted merely because a previous single seed succeeded.
The full candidate is frozen in `evaluation-continuation.json`;
[`candidate-registration-005.json`](candidate-registration-005.json) records
the choice before any completed full-panel outcome was observed. Run it with
the same runner allocation as `evaluation.json`, changing only the output
directory, then compare the complete matrices.

Completed development evidence and its limitations are retained in
[`results`](results/README.md), including failed seeds.

## Evidence and resource accounting

Each cell writes a private request, logs, `summary.json`, `resources.jsonl`, and
a `campaign` directory containing identities, progress, compact reports and
witness inputs. Failed, timed-out and required-but-unsolved runs remain visible.
Completed cell summaries and the results index are saved incrementally; an
incomplete matrix cannot be silently exported or compared as a complete panel.
The watchdog kills only its own process group after the search wall budget plus
`--finish-seconds`, or when its sampled disk footprint exceeds
`--disk-limit-gib`. The search itself stops issuing reservations at its wall
limit, then drains admitted work and verifies evidence.

- **Search quality:** verified completion, first-victory executions and emulator
  frames, objective progress/milestones, deaths and retained novelty cells. The
  novelty ledger is compacted under memory pressure and is not cumulative world
  coverage. Aggregate evidence
  can combine explored branches; it is not a claimed single trajectory.
- **Throughput:** actual admitted emulator frames / search wall time and
  executions / search wall time. Frames include snapshot-to-parent replay,
  suffix execution and admission probes. Power-on worker construction is inside
  the timed search call but excluded from its admitted frame counter. Final
  internal campaign compaction is also inside this timer. External report
  export and witness/campaign verification have separate durations.
- **Memory:** sampled process-group RSS by phase, OS maximum process RSS, logical
  archive/snapshot/index/history/draw-state charges, evictions and compactions.
  OS RSS and logical charges answer different questions and are both retained.
- **Output disk:** sampled current/peak and final logical and allocated bytes
  inside the cell output directory, categorized by stream, checkpoints, media,
  telemetry and reports/logs. The final footprint includes the summary. Shared
  assets/build bundles and runtime files elsewhere are excluded. In particular,
  QuickNES creates and unlinks private temporary core copies while their mappings
  remain live. These output metrics are not total process or host filesystem
  occupancy. Process I/O is logged separately; sampling can miss short-lived peaks.
  Broader runtime-file accounting is tracked in
  [#274](https://github.com/pH14/harmony/issues/274).
- **I/O and CPU:** sampled `/proc` read/write bytes, CPU seconds and OS block
  operation/context-switch counts. The final sampled byte totals are lower bounds
  if a process exits between samples; block operations are not byte counts.
- **Coordinator:** optional phase timing and dispatched action budgets. Requested
  replay/suffix time is labelled separately from actual emulator frame work.

Performance runs hash and discard the full event stream while retaining compact
reports and a best/winning input. This bounds disk growth without pretending a
full verification stream was saved. Each witness is replayed twice on fresh
native targets; every claimed victory must reproduce. Qualification uses
`verification: campaign`, limited to at most 5,000 executions, and additionally
re-executes the complete stream and compares the report and portable checkpoint.
These modes and verification time are explicit in the matrix. No large-run
full-campaign verification is implied by a witness-only result.

## Compare and publish

```sh
python3 benchmarks/search/eval.py compare /private/runs/baseline \
  /private/runs/candidate --out /private/comparison.json
python3 benchmarks/search/eval.py export /private/runs/candidate \
  --out /private/public/search-001
```

The export is a standalone HTML report plus allowlisted JSON/JSONL, discovered
controller inputs and SHA-256 checksums. It excludes ROMs, cores, snapshots,
full streams, private requests, arbitrary files and logs. Symlinks are rejected.
Copy the export directory to your static publication location; publishing is a
separate operator action. Source-built STB artifacts also carry the license
notice in `workloads/nes/STB-ARTIFACT-LICENSE.md` when redistributed as required.

Keep one immutable directory per revision/run. Case IDs, policy identities and
seed/resource axes support longitudinal comparisons without renaming away
regressions. Solve fractions include Wilson 95% intervals; failed infrastructure
runs are counted separately, and missing victories are censored at their budget.
The reported median victory frame count is conditional on success. Do not
replace an unsolved run with the budget as an invented completion time or infer
statistical confidence from a three-seed pilot.

Generate standalone PNG/SVG figures on the publication machine (the benchmark
host needs no plotting dependencies):

```sh
python3 -m pip install -r benchmarks/search/requirements-plots.txt
python3 benchmarks/search/plots.py --run baseline=/private/public/baseline \
  --run candidate=/private/public/candidate --out /private/public/figures-001
```

The figure generator verifies matching cells, adapters and budgets, and records
input hashes. It shows individual seeds, verified completion by frames, coverage,
throughput, process RSS, logical memory, disk, and the workload's named progress
observations. Numeric map labels are not interpreted as distances to a solution.
Each new revision can be added as another labelled series; retain the previous
run exports and figure directories unchanged. Different resource panels require
separate figures. `compare` flags changed hosts, CPU affinity and runner limits.
Dynamic scheduling can assign a seed a different CPU set when another seed
finishes first, and disjoint logical CPUs may share a physical core. Concurrent
panel timings describe that workload mix; use admitted frames for search quality
and `--jobs 1` on an otherwise quiet host for precise throughput claims. Matching
recorded environment fields alone does not prove physical isolation.

## Checks

```sh
python3 -m unittest discover -s benchmarks/search -p 'test_*.py'
cargo test --manifest-path dissonance/searcher/Cargo.toml
cargo test --manifest-path workloads/nes/Cargo.toml --lib
python3 scripts/check-dependency-boundaries.py
```

The runner tests plant failures, timeouts, disk overuse, changed assets/policies,
missing cells and publication leaks. The engine's generic continuation fixture
requires actual reuse and memory pressure before comparing live/replay results.
Licensed-ROM qualification runs locally. CI also builds Nova and STB from pinned
sources and exercises the common binary, runner, full campaign replay and compact
export; its short qualification budgets do not claim whole-game completion.

For comparisons across different suffix lengths, set `search.frames` to a
positive admitted-frame budget as well as an execution ceiling and wall limit.
Selection stops at the frame threshold and drains the existing reservation
window. Total work can therefore exceed the threshold; the overshoot is logged.
A victory first observed beyond the frame budget is preserved and verified as
evidence but does **not** pass the suite's success gate. The optional frame
budget is recorded in the deterministic header/report. Omitting it preserves
historical campaign behavior and byte format.
