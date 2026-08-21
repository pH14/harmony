<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-3 p87 normal endpoint harvest v3

Status: preregistered after the p87 result commit and before implementation
sealing, recipe materialization, ROM loading, or live candidate emulation.

## Purpose and boundary

The registered World 8-3 p54 normal harvest advanced to a live,
probe-surviving ordinary endpoint at progress 87. This v3 continuation changes
only the exact source, fresh seed, and frozen budget identity. Twelve
independent lanes receive 512 ordinary one-action draws each using the same
full-source opaque occurrence marginal and normal B1 archive machinery.

There is no button-semantic, duration-selection, route, coordinate, waypoint,
state/action association, transition-specific action, structural treatment,
or operator prior. The runner may read only the exact p87 `SmbInput` and ROM
named below. It must not read the p54 report or any earlier report, archive,
result, candidate, snapshot, stream, manifest, or recipe.

## Frozen source and seed

- Code base before this experiment and registered p87 result commit:
  `897b6f570cf183d27a7d21677907fd7a81d68601`.
- Authorizing p54 harvest preregistration: `4a19338a`; sealed implementation:
  `0318be14`; registered result commit: `897b6f57`; registered report SHA-256:
  `14bcd67c02ed672c1605a312cd461fe2a1b8e47c67c2d75a29eea61ed4974041`.
- Exact source file:
  `/root/harmony-smb-sol-w8-3-p54-harvest-4a19338a/results/adopted-world-8-3-progress-87-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `f1a3a2a396c5de1b1d4cdc8d988598a837cc05ae57de5361ed492c98a510780f`;
  exactly 111,745 bytes and 3,480 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; registered maximum and
  endpoint `SmbProgressWatermark { world: 7, level: 2, progress: 87 }`;
  exactly 163,702 frames; mechanical state `(world=7, level=2, progress=87,
  player_y_bucket=11, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,2,87,11,8,state_fingerprint=29)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `5d20b2ce16d18afb74e2302bf90cc7abd8e66755a19484fd24a0bdb1fe11618f`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `976a4d921af030bc9410d06fce5d19d9c737e049683a06ba01107ff82f23e1c4`.
- Final opaque `ButtonChord { buttons: 130, hold_frames: 101 }`. Registered
  source probe: mask `0x00` survives all 45 frames; total source-probe work 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-3-p87-normal-endpoint-harvest-v3`; label SHA-256
  `4d2d8a0e77c9eb77e14fb283683212e39a7d5f52e167b755792645e157f741bc`;
  little-endian first-eight-byte master seed `8641221823222656333`.

The standalone binary is `smb-w8-3-p87-normal-endpoint-harvest-v3`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipe generation or lane construction, replay the source once from
gameplay genesis and validate every source fact above. From a fresh restore,
reproduce the registered mask-`0x00` 45-frame survival, then restore and
re-hash the source snapshot. Compute and record the sealed trace framing and
trace hash. Any mismatch is integrity **STOP**.

## Fresh frozen occurrence recipes

There are twelve independent lanes `l=0..11`, each with 512 draws
`d=0..511`. Only after source validation, derive:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("w8-3-p87-v3-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p87-v3-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3480

selector_digest = SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p87-v3-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete source `ButtonChord` occurrence at `source_index`. There is
no retry, filter, deduplication, semantic inspection, state association,
empirical update, or outcome feedback.

Serialize the lane-major, draw-minor 6,144-element vector
`(l_u64,d_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec`; record its byte length and SHA-256. For each lane,
serialize one bare `Vec` in draw order whose element is exactly
`(d_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `l` and
any wrapper. Before lanes, require all twelve exact projection byte vectors to
be pairwise distinct; collision is integrity STOP with no retry. Freeze every
recipe before worker or lane construction. Do not outcome-deduplicate.

## Normal B1 lanes

Each lane starts a fresh archive containing only validated p87 as `id=0`,
`parent_id=None`, `created_execution=0`, directly inserted without an origin
probe. Require one active entry. Every lane uses action limit 4,096; archive
limit 513; `Frozen` key; `ProbeAtAdmission45` masks `[0x00,0x01,0x81]`;
`FewestActions`; existing `ConcentratedRecency` selection/productivity
accounting; and no waypoint, snapback, pinned window, phrase, sibling burst,
compaction, or empirical chord update. No proposed lineage may exceed
`3480+512=3992` actions.

For draw `d`, initialize fresh `StdRand(selector_seed[d])`, call real
`Archive::select_parent` once, restore and verify it, construct
`parent_input + chord[d]`, execute the one action, and process the ordinary
endpoint through snapshot, ordered probe, restore, duplicate, and admission.
Sequence and `created_execution` are `d+1`. Call `record_selection` and
`record_selection_outcome` exactly once on the selected parent. Productive
means the candidate newly allocates; cost is its exact action plus probe work.

Any non-Ok `ExitKind`, worker failure, or emulator failure is integrity STOP.
Death with `ExitKind::Ok` ends only that draw; the next draw selects normally.

## Work, report, and adoption

Use exactly twelve persistent workers, one lane per ordinal. The coordinator
buffers replies, consumes them ascending, and is the sole report writer. Any
ordinal, restore, arithmetic, accounting, or report mismatch is integrity
STOP.

The deterministic hard bound is 737,280 scheduled action frames; 829,440 live
candidate-probe frames; 163,702 source replay frames; 45 source-probe frames;
and 4,693 setup frames from thirteen targets at 361 each: **1,735,160 total**.
Require exactly 6,144 scheduled and executed candidates and 6,144 selections.
Record all work components, active counts, and maximum lineage with checked
reconciliation. Wall time is neither recorded nor a stop condition.

The create-new NDJSON order is header, source baseline, frozen recipes, lanes
ascending, adoption classification, summary. Per candidate record lane, draw,
recipe, selector, parent/input/snapshot, candidate input, endpoint
state/hashes, probe, admission, active set, accounting, and work. The header
binds preregistration, source, ROM, executable, runner sources,
recipe/projections, trace, and config hashes. `body_sha256` covers bytes through
the last pre-summary LF; after summary and LF, flush, sync, and print the
whole-file SHA-256. No host path, timestamp, or wall-clock field is permitted.

Adoption-eligible entries are final-active, newly allocated, live,
probe-surviving ordinary endpoints. Exclude source, inactive entries, deaths,
refusals/rejections, and duplicates. Rank one global champion by greatest full
target-provided lexicographic watermark; fewest actions; ascending raw
semantic input SHA-256; ascending lane; then entry id.

Verdict is **ADOPT** iff that champion is strictly greater than source full
watermark `(7,2,87)`. A lexicographically later level or world is eligible on
the identical rule; no transition-specific predicate is added. Embed the
exact input and full lane/draw/lineage/state/hash/work evidence. It is the sole
authorized next source. Otherwise verdict is **STOP** and nothing is
adoptable. Any integrity mismatch authorizes nothing. There is one registered
run, no routine replay audit, rerun, or post-hoc candidate choice.

## Registered result

The one registered run completed successfully on `msr1` under implementation
commit `3175521b`, release executable SHA-256
`d5c19cdb9f4e2b8b3d6bfd0254495207b49273abf6991db1b0546d332ee374e4`.
Its 803,325,108-byte create-new report is
`/root/harmony-smb-sol-w8-3-p87-harvest-39e37a7f/results/w8-3-p87-normal-endpoint-harvest-12x512.jsonl`,
whole-file SHA-256
`be7cc836d3849bb0e0dea5775ae4a454de1474ed505ed15c49094c571a9aeb81`
and body SHA-256
`5472dd46ba33e88dd0de73eb84abe62bbf00123336abb0c84735ed43d63dd8ef`.
The service exited successfully with empty stderr. All 6,144 candidates and
selections executed. Work reconciled to 4,693 setup, 163,702 source replay,
45 source probe, 220,441 action, and 249,227 candidate-probe frames: 638,108
total, below the registered cap.

The verdict is **ADOPT**. Lane 6 entry 337 at draw 484, parent 336, is the
deterministic champion. Its lineage is
`[0,4,8,21,27,68,70,122,185,211,222,224,225,226,227,232,233,236,237,242,244,247,267,269,282,330,336,337]`.
The exact 3,507-action candidate is alive and probe-surviving and advances the
full watermark from `(7,2,87)` to **`(7,2,187)`** at absolute frame 164,661.
Its mechanical state is
`(world=7,level=2,progress=187,y_bucket=11,engine_state=8,dead=false,flag_active=false)`.
Semantic input, endpoint WRAM, and endpoint snapshot SHA-256 values are
respectively
`34b8e7a81c5d35472361f72674693c6616d43c94522d1e2cbb9f4a7ca58c8965`,
`92a1c9958ff7eda310d3a5e605788ad638dedc64e86dbf37433c083b6521271a`,
and `088a810354fbe3e31ab2c361a3ddb4a78e42cae2f09a1f32b63c8ef25a4e9c50`.
The sole authorized next-source file is
`/root/harmony-smb-sol-w8-3-p87-harvest-39e37a7f/results/adopted-world-8-3-progress-187-input.json`,
112,605 bytes with the same semantic SHA-256.
