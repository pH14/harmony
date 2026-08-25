# Sol World 8-4 p153 room-count harvest v1

Status: preregistered after the registered p153 pinned-window source-marginal
result commit and before implementation sealing, recipe materialization, ROM
loading, or live emulation.

## Question and scope

Four registered runs from the exact p153 source stopped. The diagnosis is
shared by all of them: every action taken from progress 152 or 153 in this
area triggers the game's own page-10 loop-back, which resets progress by four
pages, and no second approach at any height passed it. The registered ordering
`(world, level, progress)` measures progress within one area only. When the
player enters a sub-area through a pipe, progress restarts near zero, so the
ordering rewards nothing about leaving the current area. Every run so far has
therefore concentrated on the same strip of the same area.

This run adds one term that does not encode a route: a count of distinct
rooms visited within the current level. It follows the same precedent as the
published Antithesis SMB and Metroid work, which gave the search only the
memory locations of position and level and rewarded every distinct value
equally. Here the search is told which two bytes identify the current room and
that any value it has not yet seen in this level is new. It is never told
which room is the right one.

It asks whether an ordinary archive continuation from p153, with the room
count added to the archive key, the selector's frontier grouping, and the full
watermark, yields a final-active live endpoint with a strictly greater full
watermark. It is a harvest, not a policy comparison: ADOPT carries one exact
verified champion; STOP authorizes no relaxation or rerun.

## Frozen provenance and source

- Code base and registered pinned-window source-marginal result commit before
  this experiment: `0e45778f30fcb7ba6855f07555db0b76af10dc6b`.
- Authorizing pinned-window source-marginal preregistration
  `e7bb0c0b7fe1c412894b997079ee954e1a6bcbc5`, implementation
  `26f14912c7c261ae2bac348e7fe483d2cb44cc2e`, report SHA-256
  `fcf8a0aaddb339f72b9e7da0ffb3c5b40be59188aaac670a483b2f9f840f6ee3`,
  and registered-result document SHA-256
  `c73278831c6309937b89f0b9633d9b45f642093e5bae05b0de472906c69e2062`.
- Launch-only source path:
  `/root/harmony-smb-sol-w8-4-p113-harvest-c765fcf4/results/adopted-world-8-4-progress-153-input.json`.
- Compact and semantic source SHA-256:
  `14af93bd006ba77cea923ab31cb7aa8ac0ad903a7bc65d5a378c92ccc337300b`;
  114,838 bytes, 3,576 actions, 168,594 frames.
- Exact endpoint watermark `(7,3,153)`; mechanical endpoint
  `{player_y_bucket:11,player_engine_state:8,dead:false,flag_active:false}`;
  Frozen key `{world:7,level:3,progress:153,player_y_bucket:11,
  player_engine_state:8,state_fingerprint:9}`, extended by this run to
  `rooms:1`; source room value `(3,5)`.
- WRAM SHA-256
  `897c7bc0df63a68249b75e81a8bfc8ea3a87a7c872241d4e51a2819ff39689c5`;
  snapshot SHA-256
  `329594d247d5a97ea59a0e7ec1b0856cfb0388141941f05062e4d6641adf5344`;
  final chord `(0x82,104)`; milestones
  `{max_1_1_scroll_bucket:195,reached_1_1_flag:true,reached_1_2:true,
  reached_onward:true}`.
- ROM SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

The binary may read only this compact input, the ROM, and its current
executable. It must not read any prior report, recipe, snapshot, or other
campaign/canary artifact. Provenance values are constants only.

## Frozen room term

- Room value: the ordered byte pair `(wram[0x074e], wram[0x074f])`, read at an
  action boundary from the same work RAM the Frozen key is decoded from. No
  other byte, no pointer, and no coordinate is read for this term.
- Room set of the source entry: `{(3,5)}`; its count `rooms` is 1. The
  baseline replay must observe exactly `(3,5)` at the source endpoint.
- Room set of a live child at an action boundary: if the child's mechanical
  `(world, level)` equals the parent's, the parent's room set plus the child's
  room value; otherwise the singleton of the child's room value. `rooms` is
  the size of that set, checked into `u8`. Only action boundaries are
  observed; rooms passed through inside one action are not counted.
- `SmbArchiveKey` gains `rooms: u8`, serialized only when non-zero so every
  existing key stays byte-identical. `archive_key` produces zero; this runner
  sets the field from the room set after decoding the Frozen key. All other
  users of the key keep zero.
- The library frontier grouping in `best_unexhausted_class` becomes
  `(world, level, rooms)` in descending order, progress-banded within the
  group exactly as before. Entries with more rooms in the same level are
  therefore the preferred tie class. Uniform pool selection, exhaustion, and
  recency are unchanged.
- Full watermark for this run is `(world, level, rooms, progress)` compared
  lexicographically. The source is `(7,3,1,153)`.
- Death, level change, and duplicate handling are unchanged. The room set
  has no effect on admission, replacement, or probe.

## Frozen recipes and mechanics

Seed label `sol-restart-w8-4-p153-room-count-harvest-v1` has SHA-256
`e7c2dc82357919ddae30e30de1576595ab996f3e15a115d051bff0f10d309dfd`;
its first eight bytes interpreted little-endian are master seed
`15931898427535573735`.

Use exactly 12 independent lanes `l=0..11`, 512 draws `d=0..511` per lane,
and 6,144 total one-action jobs. Derive lane seed as the first little-endian u64
of `SHA256(master_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)`.
For every draw independently derive:

- source occurrence index = first little-endian u64 of
  `SHA256(lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)`
  modulo 3,576;
- selector seed = first little-endian u64 of
  `SHA256(lane_seed_u64_le || ASCII("normal-endpoint-parent") || d_u64_le)`.

Copy the entire opaque source `ButtonChord` at the selected occurrence as
the draw's action. There is no retry, rejection, filtering, semantic inspection,
deduplication, adaptive table, or outcome feedback. Modulo reduction is
accepted and recorded. After the exact source replay/probe/restoration
succeeds and before lane targets or archives exist, materialize the complete
lane-major recipe Vec, its SHA/bytes, and each lane projection. The global
identity is one bare lane-major Vec of
`(lane_u64,draw_u64,source_index_u64,ButtonChord,selector_seed_u64)` tuples;
its registered SHA-256 is
`a8f98830c477ec2bed72ac6de044dc9986b1d06b656ba5650e5fb679fb094035`
over 400,216 bytes. Each lane projection is one bare draw-ordered Vec of
`(draw_u64,source_index_u64,ButtonChord,selector_seed_u64)` tuples, excluding
lane and wrappers. Require all 12 exact projection byte vectors pairwise
distinct; collision is integrity STOP without retry.

Replay the source once from genesis and verify every registered source datum.
Run a restored mask-0 45-frame source probe, require `ExitKind::Ok`, survival,
and exact restoration. Only then materialize recipes and construct lanes.

Each lane gets one persistent target and a fresh `Archive` initialized by a
trusted direct id-0 insertion of the exact source input/key/milestones/snapshot,
parent none, execution 0, with no origin probe/insertion emulation. Use action
limit 4,096, archive limit 513, Frozen key extended by `rooms`,
ProbeAtAdmission45 masks `[00,01,81]`, FewestActions replacement, absent
waypoint/snapback, and no phrase/burst/compaction/update.

Parent selection uses the real library selector policy
`PinnedWindow{world:7, level:3, low:0, high:152}`, unchanged from the
authorizing run: every draw is pinned to active entries of pair `(7,3)` whose
key progress is at most 152, with the promoted concentrated recency draw
applied within the pin and the room-count grouping above. The library falls
back to the promoted behaviour only while the pin is empty. Entries at
progress 153 or above in the source room are retained and adoptable but are
never selected as parents. An entry in a new room is selectable whenever its
progress is at most 152.

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
new ordinary endpoints, active/inactive status, parent lineage, room value and
room set, input/snapshot/WRAM hashes, probe evidence, selector accounting, and
checked work. No worker writes output.

## Bounds and frozen decision

Maximum lineage is `3,576+512=4,088 < 4,096`; source plus every possible new
allocation fits archive 513. Action work is at most `6,144*120=737,280` and
probe work at most `6,144*3*45=829,440`. Source replay is 168,594, source probe
45, and one baseline plus 12 worker setups is 4,693. The checked hard total is
**1,740,052 frames**. Wall time has no authority.

At the end consider only final-active entries newly allocated through ordinary
live probe-surviving endpoint admission; exclude source, duplicate, refused,
rejected, terminal, inactive, transient, and non-normal evidence. Rank by full
watermark `(world, level, rooms, progress)` descending, action count
ascending, semantic input SHA ascending, then lane/id ascending.

**ADOPT** iff the sole champion exists and is strictly greater than full source
watermark `(7,3,1,153)`. A second room at any progress qualifies; a later
level transition qualifies under the same lexicographic comparison. Embed its
exact `SmbInput`, hashes, endpoint/probe, room set, lane/id/parent lineage,
and work. Otherwise **STOP**. Any identity, recipe, worker, replay, probe,
archive, lineage, room, or work mismatch is integrity STOP. There is no rerun
or alternative ranking. First use of an adopted source must replay from
genesis and reproduce all evidence, including its room set, before proposals.

ADOPT does not promote the room term as a general policy for other levels; it
authorizes only the exact champion as the next source. STOP closes this exact
combination from p153 without rerun or enlargement.

World 8-4 is the final level. Terminal-like evidence (flag, credits-like
screen, or a watermark outside the registered ordering) is diagnostic only.
Completion is never declared from this run; it requires a separately frozen
mechanical completion predicate and artifact-only confirmation.

Emit create-new canonical NDJSON with header, baseline, recipes, 12 lane
records, classification, and summary, binding prereg/source/ROM/executable/bin/
module/config/recipe/trace/body/whole-file hashes. Paths, timestamps, and
completion order must not enter canonical bytes.

## Registered result

Preregistration commit `6d4c6c9d1304d2cb7230d03a1141cc3217c5af32` (document
SHA-256 `b90b6050e52ea208a53c5b4f50b2e21d170f0dec35990de3c8753321a62632b1`)
and implementation commit `12091c872eecb2cf0224109c16ab34be4fe3a9c8` used
module SHA-256
`678a3ceee9508121ee5a87b0cb02654b5f9c614ef448f2f62ad8cac9d5bfbfc7`,
bin-source SHA-256
`2e4d6b95a3c01b70e14d6d723929da345ce735a9c2983368ebab218364f038ab`,
and release-executable SHA-256
`5f78f18c080d0f48aa3adcb85662ba207052a1c85bd05244a53026aa01fd9c0f`,
built once offline and locked from sealed source archive SHA-256
`b9a3c5a619239cd5d5dca611b2914c179a87423c5afda2891ddf3eab894dc28d`
under `/root/harmony-smb-sol-w8-4-p153-room-count-12091c87`; the sealed tree
matched the implementation commit file for file. The sealed recipe was
400,216 bytes with SHA-256
`a8f98830c477ec2bed72ac6de044dc9986b1d06b656ba5650e5fb679fb094035`. The sole
run (systemd unit `harmony-smb-sol-w8-4-p153-room-count-12091c87`,
`Restart=no`, exit 0, no restarts) produced 17 NDJSON lines in registered
order, 816,667,687 bytes, whole-file SHA-256
`dba126e0fc52bb4a982606bf51ccbff7fcac9341d5745c207c72a3a17e68d82c`,
and body SHA-256
`b927c4feef265f7908ac71d723611a67ea15a06e0ef3c522af1ac522665b5f70`.
Standard error was empty and standard output bound the same report hash. The
baseline reproduced every registered source datum, including room value
`(3,5)` and full watermark `(7,3,1,153)`.

The registered verdict is **STOP**. All 6,144 scheduled candidates executed
and were selected and accounted exactly once; 5,210 final-active ordinary
entries were eligible and none exceeded `(7,3,1,153)`. The best eligible entry
is lane 10 id 428 at exactly `(7,3,1,153)`. Checked work was 4,693 setup +
168,594 source replay + 45 source probe + 266,015 action + 261,924 probe =
**701,271 frames**, below the 1,740,052-frame cap.

Diagnostic only. Every one of the 6,144 action endpoints, live or dead, read
room value `(3,5)`; no lineage ever left the source room, so the room term
was never exercised. 467 endpoints were dead; live progress never fell below
88 and engine states were only 8 and 11. Live endpoints by page were 339 on
page 5, 1,793 on page 6, 1,275 on page 7, 757 on page 8, and 1,513 on page 9.
The source marginal contains Down in 153 of 3,576 chords (4.3%), so about
264 draws carried Down at all, each from a parent chosen by frontier
concentration rather than by position. The term is sound but the one-action
source-marginal draw produced no event for it to reward.
