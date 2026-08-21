# Sol World 8-4 p113 normal endpoint harvest v1

Status: preregistered after the registered p73 regression-bridge result and
before implementation sealing, recipe materialization, ROM loading, or live
emulation.

## Question and scope

The fixed H8 existence harvest crossed the p73 regression with eight canonical
eligible boundaries and adopted an exact normal endpoint at `(7,3,113)`. This
run returns immediately to the already proven ordinary endpoint mechanism and
asks only whether a fresh bounded continuation from that exact source produces
a final-active live endpoint with a strictly greater full target watermark.

This is a harvest, not a policy comparison. It uses no H8 phrase, regression
gate, coordinate/route prior, semantic action choice, or prior-result candidate
other than the one authorized p113 source. ADOPT carries one exact verified
champion; STOP authorizes no threshold relaxation or automatic rerun.

## Frozen provenance and source

- Code base and registered p113 result commit before this experiment:
  `4d3440d4ff2265fb9869e8ab3cceff8277963b31`.
- Authorizing regression-bridge preregistration
  `ca7a7b2239a6fa6b44e1e0cb87d75a405b3c109b`, implementation
  `a8ba2346cc99c8ae78a8a419d2574c97c87dfe32`, report SHA-256
  `b33441042225e4a047178f708acc7b97e396e003b6212c065c21b314ed979abd`,
  and registered-result document SHA-256
  `6d79ffaea623cc835f617a044495840bc8b259d488da90bf4a4dbefa88fb0e45`.
- Launch-only source path:
  `/root/harmony-smb-sol-w8-4-p73-regression-bridge-a8ba2346/results/adopted-world-8-4-progress-113-input.json`.
- Compact and semantic source SHA-256:
  `0b72eafdf81670fdf40ef80dab9226ddbee7c855728661f893816789fb24239f`;
  114,388 bytes, 3,562 actions, 167,705 frames.
- Exact endpoint/maximum watermark `(7,3,113)`; mechanical endpoint
  `{player_y_bucket:11,player_engine_state:8,dead:false,flag_active:false}`;
  Frozen key `{world:7,level:3,progress:113,player_y_bucket:11,
  player_engine_state:8,state_fingerprint:59}`.
- WRAM SHA-256
  `3bcdfbb5291fdfbf94ed016a77783e6bbb4b400c3ae24dc8d73f5d3ea844a24c`;
  snapshot SHA-256
  `0e87a78fc87df608fb466cd94154e814e095dad9eb2956edfaebba7b34080f00`;
  final chord `(0x80,114)`; milestones
  `{max_1_1_scroll_bucket:195,reached_1_1_flag:true,reached_1_2:true,
  reached_onward:true}`.
- ROM SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

The binary may read only this compact input, the ROM, and its current
executable. It must not read the H8 report, recipes, snapshots, or any other
campaign/canary artifact. Provenance values are constants only.

## Frozen recipes and mechanics

Seed label `sol-restart-w8-4-p113-normal-endpoint-harvest-v1` has SHA-256
`063eb12ff0a5d06af90cc088f21ba22ff47db6949b5ae16a189a93b095ffa9c1`;
its first eight bytes interpreted little-endian are master seed
`7696834214187056646`.

Use exactly 12 independent lanes `l=0..11`, 512 draws `d=0..511` per lane,
and 6,144 total one-action jobs. Derive lane seed as the first little-endian u64
of `SHA256(master_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)`.
For every draw independently derive:

- source occurrence index = first little-endian u64 of
  `SHA256(lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)`
  modulo 3,562;
- selector seed = first little-endian u64 of
  `SHA256(lane_seed_u64_le || ASCII("normal-endpoint-parent") || d_u64_le)`.

Copy the entire opaque source `ButtonChord` at the selected occurrence. There
is no retry, rejection, filtering, semantic inspection, deduplication, adaptive
table, or outcome feedback. Modulo reduction is accepted and recorded. After
the exact source replay/probe/restoration succeeds and before lane targets or
archives exist, materialize the complete lane-major recipe Vec, its SHA/bytes,
and each lane projection. The global identity is one bare lane-major Vec of
`(lane_u64,draw_u64,source_index_u64,ButtonChord,selector_seed_u64)` tuples.
Each lane projection is one bare draw-ordered Vec of
`(draw_u64,source_index_u64,ButtonChord,selector_seed_u64)` tuples, excluding
lane and wrappers. Require all 12 exact projection byte vectors pairwise
distinct; collision is integrity STOP without retry.

Replay the source once from genesis and verify every registered source datum.
Run a restored mask-0 45-frame source probe, require `ExitKind::Ok`, survival,
and exact restoration. Only then materialize recipes and construct lanes.

Each lane gets one persistent target and a fresh `Archive` initialized by a
trusted direct id-0 insertion of the exact source input/key/milestones/snapshot,
parent none, execution 0, with no origin probe/insertion emulation. Use action
limit 4,096, archive limit 513, Frozen key, ProbeAtAdmission45 masks
`[00,01,81]`, FewestActions replacement, ConcentratedRecency selection with
fresh registered `StdRand` per draw, absent waypoint/snapback/pin, and no
phrase/burst/compaction/update.

For draw `d`, create fresh `StdRand(selector_seed)` and call the real
`Archive::select_parent(...,4096)` exactly once. Restore and verify the selected
parent, execute the one registered full action, and construct exact
`parent.input || action`. Non-Ok is integrity STOP. Ok-death is recorded and
does not stop later draws. A live endpoint receives the unchanged ordered
normal probe, with exact restore before each attempt and after completion, then
ordinary duplicate/admission/replacement handling using execution `d+1`.
Call `record_selection` and `record_selection_outcome` exactly once after the
draw on every outcome. Productive means a newly allocated entry, including
replacement; old-id duplicate/refusal/rejection/death is not productive. Cost
is exact action plus probe work.

Use 12 persistent workers, lane `l` on worker `l`, and canonical lane/draw
report order. Worker timing reaches no bytes. Preserve explicit provenance for
new ordinary endpoints, active/inactive status, parent lineage, input/snapshot/
WRAM hashes, probe evidence, selector accounting, and checked work. No worker
writes output.

## Bounds and frozen decision

Maximum lineage is `3,562+512=4,074 < 4,096`; source plus every possible new
allocation fits archive 513. Action work is at most `6,144*120=737,280` and
probe work at most `6,144*3*45=829,440`. Source replay is 167,705, source probe
45, and one baseline plus 12 worker setups is 4,693. The checked hard total is
**1,739,163 frames**. Wall time has no authority.

At the end consider only final-active entries newly allocated through ordinary
live probe-surviving endpoint admission; exclude source, duplicate, refused,
rejected, terminal, inactive, transient, and non-normal evidence. Rank by full
target watermark descending, action count ascending, semantic input SHA
ascending, then lane/id ascending.

**ADOPT** iff the sole champion exists and is strictly greater than full source
watermark `(7,3,113)`. Embed its exact `SmbInput`, hashes, endpoint/probe,
lane/id/parent lineage, and work. A later level transition is eligible under the
same lexicographic comparison. Otherwise **STOP**. Any identity, recipe,
worker, replay, probe, archive, lineage, or work mismatch is integrity STOP.
There is no rerun or alternative ranking. First use of an adopted source must
replay from genesis and reproduce all evidence before proposals.

Emit create-new canonical NDJSON with header, baseline, recipes, 12 lane
records, classification, and summary, binding prereg/source/ROM/executable/bin/
module/config/recipe/trace/body/whole-file hashes. Paths, timestamps, and
completion order must not enter canonical bytes.
