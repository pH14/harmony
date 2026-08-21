<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-3 p187 normal endpoint harvest v4

Status: preregistered after the p187 result commit and before implementation
sealing, recipe materialization, ROM loading, or live candidate emulation.

## Purpose and exclusions

The registered p87 ordinary endpoint harvest advanced to a live,
probe-surviving World 8-3 progress-187 endpoint. This v4 continuation changes
only the exact source, fresh seed, and registered identity. It uses twelve
independent 512-draw lanes with the same full-source opaque occurrence marginal
and normal B1 archive policy.

There is no button-semantic, duration-selection, route, coordinate, waypoint,
state/action association, transition-specific action, structural treatment,
or operator prior. The runner may read only the exact p187 `SmbInput` and ROM
below. It must not read the p87 report or any prior report, archive, result,
candidate, snapshot, stream, manifest, or recipe.

## Frozen source and seed

- Code base and registered p187 result commit:
  `e31195d4a9c7381e9d8bfaad82c2352261911614`.
- Authorizing p87 preregistration: `39e37a7f`; sealed implementation:
  `3175521b`; result commit: `e31195d4`; report SHA-256:
  `be7cc836d3849bb0e0dea5775ae4a454de1474ed505ed15c49094c571a9aeb81`.
- Exact source file:
  `/root/harmony-smb-sol-w8-3-p87-harvest-39e37a7f/results/adopted-world-8-3-progress-187-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `34b8e7a81c5d35472361f72674693c6616d43c94522d1e2cbb9f4a7ca58c8965`;
  exactly 112,605 bytes and 3,507 actions.
- Registered alive `ExitKind::Ok` replay maximum and endpoint watermark:
  `(7,2,187)` at exactly 164,661 frames. Mechanical state is
  `(world=7,level=2,progress=187,player_y_bucket=11,player_engine_state=8,dead=false,flag_active=false)`;
  frozen key `(7,2,187,11,8,state_fingerprint=18)`.
- Milestones: `max_1_1_scroll_bucket=195`, `reached_1_1_flag=true`,
  `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `92a1c9958ff7eda310d3a5e605788ad638dedc64e86dbf37433c083b6521271a`;
  snapshot canonical-JSON SHA-256:
  `088a810354fbe3e31ab2c361a3ddb4a78e42cae2f09a1f32b63c8ef25a4e9c50`.
- Final opaque `ButtonChord { buttons: 131, hold_frames: 116 }`; mask-`0x00`
  source probe survives all 45 frames; source-probe work is 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-3-p187-normal-endpoint-harvest-v4`; SHA-256
  `d871947407b55b2486ff95ebfe594aa284963d54f781d156b984e0d3eff78dfb`;
  little-endian first-eight-byte master seed `2619886651871359448`.

The binary is `smb-w8-3-p187-normal-endpoint-harvest-v4`, invoked as
`<input.json> <create-new-output.jsonl>` with ROM only through
`HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and 16 MiB using maximum
plus one. Before recipes or workers, replay from gameplay genesis, verify every
fact above, reproduce the source probe from an exact restore, restore again,
and re-hash. Record the sealed trace framing/hash. Any mismatch is integrity
**STOP**.

## Fresh frozen recipes

For lanes `l=0..11` and draws `d=0..511`, only after source validation derive:

```text
lane_seed = first8_le(SHA-256(
  master_seed_u64_le || ASCII("w8-3-p187-v4-lane") || l_u64_le))
source_index = first8_le(SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p187-v4-action") || d_u64_le)) mod 3507
selector_seed = first8_le(SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p187-v4-parent") || d_u64_le))
```

Copy the complete source `ButtonChord` occurrence at `source_index`. No retry,
filter, deduplication, semantic inspection, state association, empirical
update, or outcome feedback is allowed.

Serialize the lane-major 6,144-element vector
`(l_u64,d_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its byte length/SHA-256. For each lane,
serialize one bare draw-ordered `Vec` of
`(d_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `l` and
wrappers. Before workers, require all twelve exact projection byte vectors to
be pairwise distinct; collision is integrity STOP without retry. Freeze all
recipes before lane construction. Do not outcome-deduplicate.

## Normal B1 lanes

Each lane begins with only validated p187 as archive `id=0`, parent `None`,
execution 0, directly inserted without an origin probe. Require one active
entry. Use action limit 4,096; archive limit 513; `Frozen` key;
`ProbeAtAdmission45` masks `[0x00,0x01,0x81]`; `FewestActions`; real
`ConcentratedRecency` selection/productivity accounting; absent waypoint,
snapback, pin, phrase, sibling burst, compaction, and empirical update. No
proposed lineage may exceed `3507+512=4019` actions.

At draw `d`, initialize fresh `StdRand(selector_seed[d])`; call real
`Archive::select_parent` once; restore and verify it; construct
`parent_input + chord[d]`; execute one action; and process the normal endpoint
through snapshot, ordered probe, restore, duplicate, and admission. Sequence
and `created_execution` are `d+1`. Call `record_selection` and
`record_selection_outcome` exactly once. Productive means a new allocation;
cost is exact action plus probe work. A non-Ok exit or worker/emulator error is
integrity STOP. Ok-death ends only that draw; scheduling continues.

## Work, report, and decision

Use exactly twelve persistent workers, one lane each. The coordinator buffers
all replies, consumes ascending, and alone writes the report. Any ordinal,
restore, arithmetic, accounting, or report mismatch is integrity STOP.

Hard bounds: 737,280 action frames; 829,440 candidate-probe frames; 164,661
source replay; 45 source probe; and 4,693 setup frames from thirteen targets:
**1,736,119 total**. Require exactly 6,144 scheduled/executed candidates and
6,144 selections. Reconcile all counters with checked arithmetic. Wall time is
not evidence or a stop.

Create-new NDJSON order is header, baseline, frozen recipes, lanes ascending,
adoption classification, summary. Record complete recipe, selector,
parent/input/snapshot, endpoint state/hashes, probe, admission, active set,
accounting, lineage, and work evidence. Bind preregistration, source, ROM,
executable, runner sources, recipes/projections, trace, and config hashes.
Hash exact body bytes through the last pre-summary LF, then flush/sync the
complete summary-terminated file and print its SHA-256. No host path,
timestamp, or wall-clock field is allowed.

Eligibility is final-active, newly allocated, live, probe-surviving ordinary
endpoints only. Rank by greatest full target watermark, fewest actions,
ascending raw semantic input SHA-256, lane, then id. **ADOPT** iff the champion
is strictly greater than `(7,2,187)`; later level/world values qualify by the
same lexicographic rule. Embed the exact winning input and full evidence as the
sole authorized next source. Otherwise **STOP** with no adoption. There is one
run, no routine replay audit, rerun, relaxation, or post-hoc candidate choice.
