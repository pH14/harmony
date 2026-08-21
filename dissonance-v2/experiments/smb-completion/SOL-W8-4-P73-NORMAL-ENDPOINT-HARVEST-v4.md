<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-4 p73 normal endpoint harvest v4

Status: preregistered after the p73 result commit and before implementation,
recipe materialization, ROM loading, or live emulation.

## Frozen question and source

The corrected ordinary World 8-4 continuation advanced p61 to a live,
probe-surviving p73 endpoint. Continue the identical generic B1 mechanism with
a fresh seed: 12 lanes x512 one-action draws. This is the final full 512-draw
continuation that fits beneath the 4,096-action limit from this source. No
route, button, duration, coordinate, state/action association, final-level,
waypoint, transition, or semantic prior. Runtime reads only the exact p73 input
and ROM, never a report or prior artifact.

- Code/result commit `9c2b9fe634990a79112c47176049577dc436838c`.
- Authorizing prereg `3aaeb783`, implementation `9de5e622`, result `9c2b9fe6`,
  report SHA
  `6fba3d7d27bd0ab85bc0dc832f4246f7512d3d68d829eb9a48bb971857bec0bb`.
- Source
  `/root/harmony-smb-sol-w8-4-p61-harvest-v3-3aaeb783/results/adopted-world-8-4-progress-73-input.json`;
  compact/semantic SHA
  `d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c`;
  114,128 bytes; 3,554 actions.
- Alive Ok replay maximum/endpoint `(7,3,73)` at 167,340 frames;
  mechanical `(7,3,73,y=8,engine=8,dead=false,flag=false)`; key
  `(7,3,73,8,8,fingerprint=60)`; milestones `(195,true,true,true)`.
- WRAM SHA `bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734`;
  snapshot SHA `3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927`.
- Final chord `{buttons:0,hold_frames:3}`; mask00 survives exact 45-frame
  source probe. ROM SHA
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-4-p73-normal-endpoint-harvest-v4`; SHA
  `e90f4c5c70466fd96979600936a65526afce96e4943a3107af5582f2c8cd0ef4`;
  first8 little-endian master `15667819077044015081`.

Binary `smb-w8-4-p73-normal-endpoint-harvest-v4` takes input/output paths and
ROM only via `HARMONY_SMB_ROM`. Bound reads 2/16 MiB. Replay/verify all source
facts, source-probe, restore/re-hash, and record trace before recipes/workers.
Mismatch is integrity STOP.

## Recipes and execution

For lane `l=0..11`, draw `d=0..511`:

```text
lane_seed = first8_le(SHA256(master_le || "w8-4-p73-v4-lane" || l_le))
index = first8_le(SHA256(lane_seed_le || "w8-4-p73-v4-action" || d_le)) mod 3554
selector_seed = first8_le(SHA256(lane_seed_le || "w8-4-p73-v4-parent" || d_le))
```

Copy the full opaque source chord. No retry/filter/dedup/inspection/feedback.
Hash exact serde lane-major tuples `(l,d,index,chord,selector_seed)` and bare
lane projections `(d,index,chord,selector_seed)`; require all 12 byte vectors
distinct before workers, no retry.

Fresh archive per lane with source id0 only; action limit4096, archive513,
Frozen key, ProbeAtAdmission45 `[00,01,81]`, FewestActions, real
ConcentratedRecency; no waypoint/snapback/pin/phrase/burst/compaction/update.
Maximum lineage `3554+512=4066`. Every draw uses fresh selector RNG, one real
selection, verified restore, one action, normal probe/admission, and one exact
selection/outcome accounting call. Ok-death consumes the draw; non-Ok/error is
integrity STOP.

## Work and verdict

Exactly 12 persistent workers and canonical coordinator output. Require 6,144
candidates/selections. Caps: action737,280; candidate probe829,440;
replay167,340; source probe45; setup4,693; **1,738,798 total**. Checked
reconciliation; no wall authority.

Create-new NDJSON header, baseline, recipes, lanes, adoption, summary; bind all
identity/config/recipe/trace/body/file hashes without paths/timestamps.
Eligible champion is final-active, newly allocated, alive, probe-surviving
ordinary endpoint; rank full watermark, fewest actions, input SHA, lane, id.
**ADOPT** iff strictly greater than `(7,3,73)`, embedding the exact sole source;
otherwise **STOP**. One run; no rerun, relaxation, routine replay, or post-hoc
choice. Terminal-like evidence remains diagnostic pending a separately frozen
mechanical credits predicate and artifact-only confirmation.

## Registered result: STOP at World 8-4 p73

Implementation commit `c3902b4afdbff29b839637512a8902b2da036cf3`,
module SHA `c5e74707f8a00801f846311299f06356c628a50f338cd394df9d7b1b5826e93c`,
bin SHA `aaffd720de1579c732ca79c2084f472eb3390f16bd21564c2ccf23e9af1e5d3a`,
source archive SHA
`c8b9d7d27a555aa94b042521004ac6356e831183cc66978f1baa558370a07a9f`,
and release executable SHA
`e4557737c606058cb316dee5d813be33791016df6218a0bd4787bb5b0da7c7ef`
were sealed before the single launch. Systemd invocation
`5f7caab79c4f4d6fb6383342ded32a9e` exited successfully with empty stderr.

The 17-line, 800,113,754-byte report SHA is
`5fc888c8fcb522b9b1216de9649223cebbddbf87709e68d1236a4e2031ff2e90`;
body SHA is
`bc39c8c01c796eaf8ba7da9d46bec3598ee906aead06cd310e82cd79b66c9d2f`.
All 6,144 candidates and selections completed. Work was setup4,693,
source-replay167,340, source-probe45, action282,105, candidate-probe273,639,
experimental555,744, total727,822 frames, below every frozen cap.

The registered verdict is **STOP**. Across 2,920 final-active newly allocated
eligible endpoints, none exceeded the immutable p73 source floor. The best
eligible new endpoint was lane4/id198 at `(7,3,31)`, so it is not adoptable and
no new source file was materialized. This closes another identical full-size
ordinary continuation from p73. Any further work requires a separately
preregistered structural test; the registered p73 source remains the only
authorized frontier.
