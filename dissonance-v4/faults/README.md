<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance v4 fault adapter

`v4-faults` runs the v4 engine against a replicated system packaged as a
Harmony OCI image. `workloads/faults` executes every fault action in a
deterministic VM. The engine sees unnamed scalars and bytes; this adapter is
the only place that knows assertion messages and fault kinds.

## Actions

The vocabulary has `2K` inputs, where `K` is the number of fault kinds the
image offers (`faults_workload::archive::alternatives`). Inputs `0..K` wait.
Input `K + i` is fault kind `i`. Holds are off, so one engine action is one
fault action and one unit of work.

The action argument seeds a RomuDuoJr generator. The adapter draws the action
window first, `2^U(0..10)` supervisor ticks of 10 ms from
`DurationPolicy::draw_without_history`, then the kind's own parameters through
`faults_workload::archive::alternative_action`, which uses the same parameter
draws as the v3 fault sampler. An event kill or event park becomes a wait when
no live node is stopped at an instrumented event.

## Observation

Scalars, in order:

1. Eleven lifecycle values from `faults_workload::archive::archive_key`:
   `hooks_finished`, `hooks_running`, `alive`, `event_ready`,
   `event_kill_fires`, `event_park_fires`, `checks_finished`,
   `checks_running`, `workload_running`, `park_held`, `kill_armed`. Counts cap
   at 8 as in the key. Kind `identity`.
2. One value per Sometimes or Reachable assertion: 1 once it has passed on the
   timeline, else 0. Kind `identity`.
3. One value per numeric guidance id: the best value on the timeline, mapped to
   `u64` with the sign bit flipped so the order is numeric. Kind `higher` for
   a maximized id, `lower` for a minimized one. An id with no value yet reads
   as the worst value.

Assertions and guidance ids are sorted by message. The list comes from the
declarations at setup, so an assertion first declared after setup (the kit's
check assertions, for example) has no scalar. Its violation still ends the job
as a finding.

The byte array holds one bit per Sometimes or Reachable assertion that passed,
one bit per Always, AlwaysOrUnreachable or Unreachable assertion that was
violated, then one byte per key of `FaultObservations.state` in key order: the
bit length of the latest value, 0 for a value at or below 0. Targets report
relative values there (a role code, a lag, a count), so the byte tells apart
states whose scalars tie, such as a follower one entry behind and one
hundreds of entries behind. The edge digest and edge crossing count change at
almost every action and stay out of the array.

## Snapshots and workers

A snapshot handed to the engine is an eight-byte handle into an in-memory
table of `FaultSnapshot` values. Each entry records its parent handle and the
engine actions since the parent, so any handle has a route from setup. The VM
state lives in the Consonance checkpoint cache and is rebuilt from the fault
history when it was evicted.

Each worker owns one `FaultTarget` on its own thread, because the live session
is thread local. Every worker boots the image, and all must reach the same
setup receipt and observation. A finding, a failed action or a watchdog cutoff
ends the job, and that state is not offered to the engine.

## Reports

`run` writes into `--out`:

- `progress.jsonl`: one line per batch with the engine summary, executions,
  hung jobs, findings, Sometimes hit, snapshot handles and resident memory.
- `result.json`: the engine outcome (stop reason, work, jobs, job stream hash),
  executions, the layout, every Sometimes with its first pass, the best value
  of every guidance id among admitted states, and the findings.
- `bug-N.json`: one `BugReport` per finding, at most 64.
  `harmony debug run IMAGE --actions bug-N.json` replays one.
- `cells.jsonl`: the engine archive report.

A worker records a pass at the first action where the assertion reads passed,
whether or not the engine admits that state. The engine passes each job its
batch number and its index in the batch, which is the order the engine admits
the batch's jobs in. The adapter uses this order to give each first pass its
execution number (the count of engine jobs up to and including that job) and
its exact work. At every batch end it checks that each index of the batch ran
once and that the admitted jobs' work adds up to the engine's count. A pass in
a job the engine drops once the budget is reached does not count.
`passing_jobs` counts the jobs in which the assertion went from not passed to
passed. `first_admitted_work` is the engine work of the first admitted state
with the scalar set. `executions_run` counts every job that ran, including the
dropped ones.

## Replay

`replay --result DIR/result.json` boots a fresh instance and requires the same
layout. It replays each first-pass route from setup, translating every engine
action with the live `event_ready`, and checks that the assertion passed, that
the scalars match and that the fault actions match. It then replays the fault
actions of every `bug-N.json` and checks the violations and the stop reason.
Each route prints one JSON line. The command fails when any route differs.

## Running

VM runs need Linux with KVM and a static executable. The binary copies itself
into a sealed memfd and re-executes from it before it opens a session, and the
session workers it launches inherit that sealed image.

```sh
cargo build --release --manifest-path dissonance-v4/Cargo.toml \
    --target x86_64-unknown-linux-musl -p dissonance-v4-faults
v4-faults run --image /abs/path/etcd.oci --kernel bzImage \
    --base initramfs-oci.cpio.gz --seed 1 --workers 3 --budget 20000 \
    --out corpus.local/etcd-seed-1
v4-faults replay --image /abs/path/etcd.oci --kernel bzImage \
    --base initramfs-oci.cpio.gz --result corpus.local/etcd-seed-1/result.json
```

| flag | meaning |
|---|---|
| `--image` | absolute path of the prepared OCI archive |
| `--kernel`, `--base` | guest kernel and base initramfs |
| `--ram-mib` | guest memory, default 1024 |
| `--knob` | a workload knob, repeatable |
| `--boot-cache` | directory for the boot snapshot cache |
| `--cache-mib` | checkpoint cache budget, default 4096 |
| `--seed` | engine seed |
| `--workers` | parallel VMs; the job stream does not depend on it |
| `--budget` | work budget in fault actions |
| `--base-steps` | base job length in actions, default 64 |
| `--root-only` | random arm: every job starts at setup with the base length |
| `--memory-limit` | resident bytes; above it the engine stops with `memory` |
| `--out` | output directory; it must not already hold a result |
| `--result`, `--id` | replay input, and optional assertion ids to replay |

Put run directories on the lab host or under `dissonance-v4/corpus.local/`.
