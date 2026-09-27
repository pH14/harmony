# Tools for game iteration

Every command runs from the repository root. The common runner and its manifest
format are described in `benchmarks/search/README.md`; this file covers the
parts the iteration process uses.

## Contents

- Builds and assets
- Power-on runs with checkpoints
- Rooted segments and the ladder
- Checkpoint slices
- What a resume accepts
- Reading results
- Films, probes and heatmaps
- Regression checks

## Builds and assets

```sh
python3 benchmarks/search/eval.py build --out /private/builds/candidate --jobs 24
```

The build writes `nes-eval`, `nes-film` and `build-info.json`. Build the
unchanged searcher the same way as the baseline. Each build and each run output
directory must be new.

The runner reads a private asset inventory with the core and ROM paths and
their SHA-256 values; `benchmarks/search/README.md` shows its shape. ROMs,
cores, run directories and manifests with private paths stay outside the
repository.

## Power-on runs with checkpoints

Write a private manifest. Both arms of a comparison use the same manifest with
different `--binary` builds. Size workers, memory and budgets for the host.

```json
{
  "format": "harmony-search-eval-v1",
  "id": "metroid-poweron-checkpointed",
  "seeds": [101, 102, 103],
  "workers": [12],
  "memory_mib": [20480],
  "search": {
    "executions": 300000000,
    "frames": 40000000000,
    "wall_seconds": 259200,
    "window": 2,
    "result_slots": 2,
    "suffix": "one_to_six",
    "mixture": "energy_splice:6",
    "verification": "witness",
    "checkpoint_every": 1000000,
    "checkpoint_on_progress": true
  },
  "cases": [
    {
      "id": "metroid-full",
      "game": "metroid",
      "origin": "power-on new-game genesis",
      "rom_sha256": "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6",
      "search": {}
    }
  ]
}
```

```sh
python3 benchmarks/search/eval.py run /private/manifests/metroid-poweron.json \
  --assets /private/assets.json --binary /private/builds/candidate/nes-eval \
  --build-info /private/builds/candidate/build-info.json \
  --out /private/runs/metroid-poweron-candidate --jobs 2 --cpus 24 \
  --memory-capacity-mib 50000 --disk-limit-gib 400
```

- Set `frames` above `executions` times the game's frames per try, or the
  frame budget ends the run first. A Metroid power-on run averages about 110
  frames per try.
- A cell starts only when its workers fit in the free CPUs of `--cpus` and its
  `memory_mib` plus overhead fits in `--memory-capacity-mib`.
- Raise `--disk-limit-gib` above the default of 8. Checkpoints and their
  snapshot store count toward it, and the runner kills a cell that exceeds it.
  A Metroid `.ckpt` file holds up to a few hundred MB and the snapshot store
  grows by about 1 GB per million early tries.
- Each cell writes to `<matrix>/<case>-s<seed>-w<workers>-m<memory>/campaign/`.
  Checkpoints go to `checkpoints/` there as `<executions>-<reason>.ckpt`, where
  the reason is `milestone`, `top_progress` or `interval`. Milestone and
  top-progress checkpoints are kept; only the last two interval checkpoints
  are kept.
- All checkpoints in a directory share one `snapshots.store`. Copy or keep the
  whole `checkpoints/` directory, never a lone `.ckpt` file.
- The start point for a hard part is the first `-milestone.ckpt` whose
  execution count is at or after the first try of the milestone that opens it.

## Rooted segments and the ladder

A Metroid case with `root_input` starts from the state that input reaches from
power-on, with an empty archive. `{seed}` in the path is replaced with the
cell's seed.

```json
{
  "id": "tourian-from-stocked",
  "game": "metroid",
  "origin": "best-stocked tourian arrival from the power-on baseline",
  "rom_sha256": "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6",
  "root_input": "/private/roots/tourian-energy.json",
  "search": {}
}
```

- Take roots from `campaign/milestone-inputs/` of a finished cell. `NAME.json`
  is the first arrival; `NAME-energy.json`, `NAME-missiles.json` and
  `NAME-boss.json` are the best-stocked arrivals. A finished run replays every
  tape in that folder twice and checks it reaches its milestone. Probe a tape
  with `metroid-map-probe` to read its resources before rooting at it.
- A tape from a power-on run is a full input from power-on and roots directly.
  A tape from a rooted run starts at its root. Rooting at it needs the full
  input: the root's actions, the idle actions for the frames the rooted start
  waited, then the tape. Check the joined input with `metroid-map-probe`; it
  must reach the same endpoint as the tape probed with `--root`.
- `benchmarks/search/metroid-ladder.json` holds one case per Metroid chain root
  on three seeds. `eval.py ladder` scores matrices run over it: a milestone
  passes when two distinct seeds reach it, and the score counts passed
  milestones in a row from each root. Columns are builds, so a gain at one root
  and a loss at another show on one table.

```sh
python3 benchmarks/search/eval.py ladder /private/runs/ladder-baseline \
  /private/runs/ladder-candidate --out /private/ladder-scores
```

- `eval.py compare` refuses to combine rooted and power-on cells for one case,
  so compare segments only with segments.

## Checkpoint slices

A case with `resume` continues the search saved in that checkpoint. A resume
with the original seed repeats the original progress lines; each other seed
keeps the archive and draws new choices. A slice therefore runs the manifest's
new seeds from one checkpoint.

```json
{
  "format": "harmony-search-eval-v1",
  "id": "tourian-slice",
  "seeds": [201, 202, 203],
  "workers": [12],
  "memory_mib": [20480],
  "search": {
    "executions": 70000000,
    "frames": 40000000000,
    "wall_seconds": 86400,
    "window": 2,
    "result_slots": 2,
    "suffix": "one_to_six",
    "mixture": "energy_splice:6",
    "verification": "witness",
    "checkpoint_on_progress": true
  },
  "cases": [
    {
      "id": "metroid-full",
      "game": "metroid",
      "origin": "resume at the tourian milestone checkpoint of seed 101",
      "rom_sha256": "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6",
      "resume": "/private/runs/metroid-poweron-base/metroid-full-s101-w12-m20480/campaign/checkpoints/000065700000-milestone.ckpt",
      "search": {}
    }
  ]
}
```

- `executions` and `frames` count from the original power-on. Set
  `executions` to the checkpoint's execution count plus the slice length.
- Keep the worker count, window and memory of the original run.
- Run the same manifest with the changed build and with the unchanged build.
  The legs are tries after the resume: a milestone's first try minus the
  checkpoint's execution count, which is the number in the `.ckpt` file name.
  `milestones.py` prints first tries from power-on, so subtract by hand.
- Before the slices, resume the checkpoint with its original seed on the
  unchanged build for a short stretch. Each progress line before its last must
  match the original run's line at the same try, apart from the timing fields
  `search_elapsed_millis` and `unix_time`. The last line differs, because a
  finished run writes its final census there.
- A resumed stream cannot be replayed, so resumed runs use witness
  verification.
- Both arms of a slice must read the same checkpoint. A change that alters the
  checkpoint format cannot be tested with slices; use rooted segments and a
  power-on run.

## What a resume accepts

A resume may change the suffix, mixture and retention policies, the selector,
the continuation policy, the objective stop, the draw table policy, the
preference portfolio and a workload's `preference_policy`. The draw tables and
slot holders are rebuilt from the archive entries, and the origin record and
stream header list each change as `checkpoint_policy_changes`. A change to one
of these policies must change that policy's identifier, or the resume records
no change.

A resume refuses a changed workload identity, controller vocabulary, key
extraction, duration, replacement, terminal or emulator policy, worker count,
admission window (`window`), archive entry limit or memory budget
(`memory_mib`). The execution, frame and wall budgets may change. Test a change
the resume refuses with rooted segments and a new power-on run instead of
slices.

## Reading results

```sh
python3 benchmarks/search/milestones.py /private/runs/metroid-poweron-base \
  /private/runs/metroid-poweron-candidate --out /private/milestones
python3 benchmarks/search/eval.py compare /private/runs/metroid-poweron-base \
  /private/runs/metroid-poweron-candidate --out /private/comparison.json
```

- `milestones.py` prints the first try at each milestone per build, seed and
  cell. It reads Metroid cells and a subset of Metroid's milestones; for
  `kraid_door`, the boss rooms or another game, read
  `workload_diagnostics.named_progress.first_seen` from the last line of
  `campaign/progress.jsonl`, or add the milestones to the script.
- The progress line at every 100,000th try carries `selector.draws_by_cell`,
  the draws each place has received. Every line carries
  `selector.tier_draws_by_rank`. These are the data for a heatmap while a run
  is going or after it was stopped.
- The final progress record's `retained_diagnostics` holds the archive census,
  written only when a run finishes:
  `live_entries_by_map_cell` maps `area:map_x:map_y` to entries, best health,
  best missiles, the equipment union and selections, and
  `live_entries_by_map_cell_and_equipment` splits it by equipment. The census
  shows where the archive sits and what its states can do there.
- Keep exports and `milestones.py` output in an untracked directory such as
  `benchmarks/search/exports/`, never in a commit.

## Films, probes and heatmaps

```sh
cargo build --release --locked --manifest-path workloads/nes/Cargo.toml \
  --bin metroid-film --bin metroid-map-probe
export HARMONY_METROID_ROM=/private/assets/metroid.nes
export HARMONY_QUICKNES_CORE=/private/assets/quicknes_libretro.so
workloads/nes/target/release/metroid-film TAPE.json out.mp4
workloads/nes/target/release/metroid-map-probe TAPE.json --wram 0:256
```

Add `--root ROOT.json` to either tool only for a tape from a rooted run.

- Film a cell's witness, `campaign/witness-input.json`, and its milestone
  tapes with `metroid-film`: the first arrival and the best-stocked arrival.
  `eval.py film` renders through `nes-film`, which covers the Nova and STB
  benchmark scenarios only.
- A power-on run keeps inputs on disk only for the witness and the milestone
  tapes. To film other archive states, such as the deepest holder in a stalled
  area, the run or a checkpoint reader has to write their inputs out.
- A rooted witness is only the part after the root, so film a rooted tape with
  `--root` and watch the milestone tapes, which carry the route.
- `metroid-map-probe` prints the map cell and resources at each action endpoint.
  `--wram START:LEN` also prints the 2 KiB internal RAM from START, in hex,
  for LEN bytes, in decimal. Reading defects show up in these bytes.
- `nes-progress metroid CORE ROM TAPE.json` replays a tape from power-on and
  reports its named milestones.
- Mega Man 2 has `mm2-film <stage> <chain-prefix.json> <input.json>
  <output.mp4>`, `mm2-probe` and `mm2-energy-probe` in `workloads/nes/src/bin`.
  They replay one stage from a chain prefix, so a whole-game mode needs them
  extended.
- A heatmap is a short private script that draws `selector.draws_by_cell` or
  the census per map cell on the game's map as an image. Open the image and
  look at it before reading the numbers behind it.

## Regression checks

```sh
python3 benchmarks/search/eval.py run benchmarks/search/smb-regression-three.json \
  --assets /private/assets.json --binary /private/builds/candidate/nes-eval \
  --build-info /private/builds/candidate/build-info.json \
  --out /private/runs/smb-three-candidate --jobs 1 --cpus 24 \
  --memory-capacity-mib 8192
```

- Run the same manifest with a main build into another output directory, then
  `python3 benchmarks/search/eval.py compare MAIN_RUN CANDIDATE_RUN --out
  /private/smb-compare.json`. Every SMB cell must solve. Compare
  `executions_to_first_victory`. The three cells run one at a time at 24
  workers, up to 30 minutes each.
- The etcd case, `workloads/bugs/historical/etcd-3.5-inconsistency`, runs in
  the historical bugs workflow. Dispatch it on the change's branch with
  `gh workflow run harmony-workloads-historical-bugs.yml --ref BRANCH`, which
  runs every historical case, and compare the etcd job with main's nightly run
  of the same workflow. It must still find the bug.
- The Dissonance review lens in `REVIEWING.md` asks for CPU and memory costs.
  `uv run workloads/tiny-worlds/scale.py slowdown --binary before --binary after`
  measures the slowdown; the other `scale.py` modes measure core scaling and
  memory.
