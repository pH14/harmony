<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-4 p61 normal endpoint harvest v3

Status: preregistered after the v2 integrity-STOP result commit and before v3
implementation, recipe materialization, ROM loading, or live emulation.

## Frozen question and source

The first ordinary World 8-4 harvest advanced from p0 to a live,
probe-surviving p61 endpoint. The v2 continuation integrity-STOPped during
source validation because its frozen source-probe transcript was copied
incorrectly; it did not materialize runtime recipes, construct workers, or
execute an experimental proposal. Run the identical generic B1 mechanism with
a fresh seed: 12 lanes x512 one-action draws. No route, button, duration,
coordinate, state/action association, final-level, waypoint, transition, or
semantic prior. Runtime reads only the exact p61 input and ROM, never a report
or prior artifact.

- Code/result commit `44978d2521da3a043aed121a8c16af4f611a3676`.
- Authorizing p0 prereg `4f0e7549`, implementation `597ea67f`, result
  `6bd11649`, report SHA
  `255f9b430841303a4e5d9c9d6eb9820c1887ba9c7b3c3f5192d53c2c1eb87e59`.
- Failed v2 prereg `97b9f4be`, implementation `f5bd5c38`, result `44978d25`;
  its empty report/output SHA is
  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`
  and stderr SHA is
  `d1d590e5df74a7cbf24970f98b8d5e34f49970adbe399e83a3db1c3308b9241e`.
- Source
  `/root/harmony-smb-sol-w8-4-p0-harvest-4f0e7549/results/adopted-world-8-4-progress-61-input.json`;
  compact/semantic SHA
  `15572c6ea86e749d89995a74ee725bf76a5da500b14efa508635d3e2f664da4c`;
  113,972 bytes; 3,549 actions.
- Alive Ok replay maximum/endpoint `(7,3,61)` at 167,136 frames;
  mechanical `(7,3,61,y=11,engine=8,dead=false,flag=false)`; key
  `(7,3,61,11,8,fingerprint=58)`; milestones `(195,true,true,true)`.
- WRAM SHA `fae8453cb375f25a913d34d2c3aaf8d9d5d5fd109269eaf845d2c0a6cec9781e`;
  snapshot SHA `62761b7a01aa2d942ea44da20b814e657fb20ff4036f918738c2c8d980e914be`.
- Final chord `{buttons:0,hold_frames:11}`. From an exact restored source
  snapshot, the ordered source-probe transcript is
  `(mask=00,work=35,dead=true,survived=false)`, restore, then
  `(mask=01,work=45,dead=false,survived=true)`, total 80 frames; restore and
  revalidate the exact source afterward. ROM SHA
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-4-p61-normal-endpoint-harvest-v3`; SHA
  `e00038fb87e276f52748629563ac566802fb8e4ffa5753d303d1555e07839617`;
  first8 little-endian master `17687573660207415520`.

Binary `smb-w8-4-p61-normal-endpoint-harvest-v3` takes input/output paths and
ROM only via `HARMONY_SMB_ROM`. Bound reads 2/16 MiB. Replay/verify all source
facts, execute and record the exact ordered source probes with restoration,
restore/re-hash, and record trace before recipes/workers. Mismatch is integrity
STOP.

## Recipes and execution

For lane `l=0..11`, draw `d=0..511`:

```text
lane_seed = first8_le(SHA256(master_le || "w8-4-p61-v3-lane" || l_le))
index = first8_le(SHA256(lane_seed_le || "w8-4-p61-v3-action" || d_le)) mod 3549
selector_seed = first8_le(SHA256(lane_seed_le || "w8-4-p61-v3-parent" || d_le))
```

Copy the full opaque source chord. No retry/filter/dedup/inspection/feedback.
Hash exact serde lane-major tuples `(l,d,index,chord,selector_seed)` and bare
lane projections `(d,index,chord,selector_seed)`; require all 12 byte vectors
distinct before workers, no retry.

Fresh archive per lane with source id0 only; action limit4096, archive513,
Frozen key, ProbeAtAdmission45 `[00,01,81]`, FewestActions, real
ConcentratedRecency; no waypoint/snapback/pin/phrase/burst/compaction/update.
Maximum lineage `3549+512=4061`. Every draw uses fresh selector RNG, one real
selection, verified restore, one action, normal probe/admission, and one exact
selection/outcome accounting call. Ok-death consumes the draw; non-Ok/error is
integrity STOP.

## Work and verdict

Exactly 12 persistent workers and canonical coordinator output. Require 6,144
candidates/selections. Caps: action737,280; candidate probe829,440;
replay167,136; source probe80; setup4,693; **1,738,629 total**. Checked
reconciliation; no wall authority.

Create-new NDJSON header, baseline, recipes, lanes, adoption, summary; bind all
identity/config/recipe/trace/body/file hashes without paths/timestamps.
Eligible champion is final-active, newly allocated, alive, probe-surviving
ordinary endpoint; rank full watermark, fewest actions, input SHA, lane, id.
**ADOPT** iff strictly greater than `(7,3,61)`, embedding the exact sole source;
otherwise **STOP**. One run; no rerun, relaxation, routine replay, or post-hoc
choice. Terminal-like evidence remains diagnostic pending a separately frozen
mechanical credits predicate and artifact-only confirmation.
