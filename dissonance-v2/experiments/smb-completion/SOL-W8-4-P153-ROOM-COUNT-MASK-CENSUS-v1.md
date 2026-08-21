# Sol World 8-4 p153 room-count mask census v1

Status: preregistered after the registered p153 room-count harvest result
commit and before implementation sealing, ROM loading, or live emulation.

## Question and scope

The room-count harvest stopped with every one of its 6,144 action endpoints
still in room `(3,5)`. The room term was never exercised because no draw
produced a room change. A draw needs two things at once: a parent standing
where a room change is possible and an action that causes it. The source
marginal carries Down in 4.3% of chords and parents are chosen by frontier
concentration, so the pair is rare.

This census removes the draw. It reproduces the twelve registered harvest
archives exactly and then, from every final-active entry, executes every one
of the 14 source masks held for the maximum 120 frames. It asks one question:
does any single full-length action from any retained state leave room `(3,5)`
or otherwise exceed full watermark `(7,3,1,153)`? The mask set is the source
support already sealed by the regression-bridge census; the duration is the
registered chord maximum. No mask is preferred, no parent is preferred, and no
position or route is consulted.

It is a census with an adoption rule, not a policy comparison. ADOPT carries
one exact verified champion; STOP authorizes no relaxation or rerun.

## Frozen provenance and source

- Code base and registered room-count harvest result commit before this
  experiment: `531fae84518f53980e25393e06a5b97a8395b9ea`.
- Authorizing room-count harvest preregistration
  `6d4c6c9d1304d2cb7230d03a1141cc3217c5af32`, implementation
  `12091c872eecb2cf0224109c16ab34be4fe3a9c8`, report SHA-256
  `dba126e0fc52bb4a982606bf51ccbff7fcac9341d5745c207c72a3a17e68d82c`,
  and registered-result document SHA-256
  `aa7f6c2c94086faf39202d92477dc9b287451d877fe2591ed643a0ed85ccc771`.
- Launch-only source path, source hashes, source endpoint, Frozen key with
  `rooms:1`, source room `(3,5)`, WRAM/snapshot hashes, final chord,
  milestones, and ROM hash: unchanged from the room-count harvest
  preregistration.

The binary may read only the compact source, the ROM, and its current
executable. It must not read the room-count report. The registered per-lane
final-entry digests below are constants only.

## Frozen harvest reproduction

Seed label, master seed, recipe derivation, recipe identity SHA-256
`a8f98830c477ec2bed72ac6de044dc9986b1d06b656ba5650e5fb679fb094035`
(400,216 bytes), lane projections, baseline replay and probe, archive
configuration, room term, selector policy, draw mechanics, admission, and
selector accounting are exactly those of the room-count harvest. Each lane
reruns its 512 draws unchanged.

After its draws, each lane serializes its final-active entry records with
`serde_json::to_vec` and requires the SHA-256 of those bytes and the entry
count to equal the registered values:

| lane | entries | final-entry SHA-256 |
|---:|---:|---|
| 0 | 468 | `59f0f243d761526c6be332adbfb30177e36d26e683ad4a1c924fbd2c3261a4a2` |
| 1 | 383 | `8bb95455c719fb6ac0a7e5ba6d313b7f3b37f006e91f0ca2e9a232bde409d3d9` |
| 2 | 433 | `3e9de6122428b421533b2cc70b61ee213498bcfc3d43f0b97cbce6660bb96173` |
| 3 | 435 | `831c5e36d0eea3860f641d4b73569c11ee2bed505100577cc2c163e39721d7b9` |
| 4 | 454 | `e1355a0919a39426af04a62a56b5fe52a03e3bdef76982bbe6ca981b1c3deee6` |
| 5 | 433 | `f11923a0cb7e3a346562aefaa4d6c3a0b293b2439455f0fd8b85109bc27b205c` |
| 6 | 394 | `92c02e0b5d795e3273ae4b0f4b475793459ecc95c54673876774db101d1338cc` |
| 7 | 455 | `dc3ef6812a29175eb340c39a7151ee93994f4e5588a954c62075696ab21658c7` |
| 8 | 493 | `6895dfbe182e4813cfcfe1743dfc45762210e68116b7226257f2cfea82fe10d6` |
| 9 | 390 | `06ec7fb18300ed0c29938b67673e8d29802a7713c84ea1b680bb9301d0eae864` |
| 10 | 435 | `e6d2fdc960536ee1d17039d23012f9ccdc43489972714fa840524cf71b343eb2` |
| 11 | 437 | `0cd4084e9190d03d69d296f10cb243182ed849b7cb6f9f64ee51bcf3bde86855` |

Registered harvest work per lane (action, probe) in frames: 0 (21,059,
22,204); 1 (20,052, 19,544); 2 (23,920, 22,411); 3 (22,732, 22,050);
4 (21,656, 21,689); 5 (20,472, 23,479); 6 (20,731, 19,441); 7 (22,936,
22,518); 8 (23,907, 22,820); 9 (22,065, 21,760); 10 (20,475, 21,251);
11 (26,010, 22,757). Each lane requires its reproduced harvest work to equal
these values. Any digest, count, or work mismatch is integrity STOP before
the census begins.

## Frozen census

Masks in exact ascending order `[0,1,2,16,32,64,66,128,129,130,131,192,193,
194]`; one duration, 120 frames. For each lane, for each final-active entry in
ascending id order, for each mask in order: restore and byte-verify the
entry's retained snapshot, execute the single chord `(mask,120)`, and record
the boundary: entry id, mask, parent input SHA, exact cumulative input SHA
and action count, requested and actual work, observation, mechanical state,
room value, room set, `rooms`, full watermark, Frozen key with `rooms`, raw
WRAM SHA, death. Non-Ok is integrity STOP. Ok-death is recorded. The room set
rule is the harvest rule applied to the entry's registered room set.

Only a live boundary whose full watermark is strictly greater than
`(7,3,1,153)` receives the unchanged ordered normal probe, masks
`[00,01,81]`, 45 frames each, with exact restore before every attempt and
after completion. Other live boundaries are not probed. After every boundary,
restore and verify the entry snapshot. No boundary is inserted into any
archive; there is no selector, replacement, or cross-boundary state.

Each lane's census runs on the lane's own worker after its harvest, in
canonical order. Report order is lane-major then id then mask. Completion
timing reaches no bytes.

## Bounds and frozen decision

Harvest work is bounded as registered: action at most 737,280 and probe at
most 829,440 frames. Census boundaries total `5,210*14=72,940`; census
action work is at most `72,940*120=8,752,800` frames and conditional probe
work at most `72,940*135=9,846,900` frames. Source replay is 168,594, source
probe 45, and one baseline plus 12 worker setups is 4,693. The checked hard
total is **20,339,752 frames**. Maximum input length is `3,606+1=3,607`,
below the 4,096 action limit. Wall time has no authority.

Eligible evidence is a census boundary that is `ExitKind::Ok`, alive,
strictly greater than `(7,3,1,153)` in full watermark, and probe-surviving.
Harvest entries are reproductions of registered evidence and are not
eligible. Rank eligible boundaries by full watermark descending, action count
ascending, semantic input SHA ascending, then lane, id, and mask ascending.

**ADOPT** iff at least one eligible boundary exists; embed the sole
champion's exact `SmbInput`, hashes, endpoint, room set, probe, lane/id/
mask, lineage through the reproduced archive, and work. Otherwise **STOP**.
Separately and diagnostically, classify **ROOM_EXIT_OBSERVED** iff any
boundary, live or dead, reads a room value other than `(3,5)` while still in
`(7,3)`; otherwise **NO_ROOM_EXIT**. Any identity, digest, replay, probe,
boundary, or work mismatch is integrity STOP. There is no rerun or alternative
ranking. First use of an adopted source must replay from genesis and
reproduce all evidence, including its room set, before proposals.

STOP with NO_ROOM_EXIT closes single-action room exit from every state these
twelve archives retained. It does not close multi-action continuations and
does not reopen any rejected mechanism. World 8-4 is the final level;
terminal-like evidence is diagnostic only and completion is never declared
from this run.

Emit create-new canonical NDJSON with header, baseline, recipes, 12 lane
records (harvest draws, final entries, census boundaries), classification,
and summary, binding prereg/source/ROM/executable/bin/module/config/recipe/
trace/body/whole-file hashes. Paths, timestamps, and completion order must
not enter canonical bytes.
