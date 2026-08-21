<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p213 normal endpoint transition harvest v1

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Purpose and boundary

The registered B1-vs-B2 canary independently adopted a live normal endpoint at
World 8-2 progress 213. Its target-observed mechanical baseline has
`flag_active=true`, but B2 was not promoted and that comparison cannot repeat.
This short harvest therefore uses only the registered B1 policy and full-source
opaque occurrence marginal to seek a strict full-watermark advance or level
transition.

The flag bit is baseline integrity evidence only. It must never enter proposal,
selection, admission, ranking, adoption, stopping, or any outcome-dependent
decision. There is no button-semantic, duration-selection, route, coordinate,
waypoint, state/action-association, transition-specific action, or operator
prior.

The binary may read only the exact p213 `SmbInput` and ROM named below. It must
not read the B1-vs-B2 report or any earlier report, archive, result, candidate,
snapshot, stream, manifest, or recipe.

## Frozen source and seed

- Code base before this experiment and registered p213 result commit:
  `0f3bd9de846b5a455c4809402a1efb2976732307`.
- Authorizing B1-vs-B2 preregistration: `ee578070`; sealed implementation:
  `762a03a0`; registered result commit: `0f3bd9de`; registered report SHA-256:
  `a8f8edee757d5328c21cd3485f0671ad398540dcbd2668a9002661812d6aff21`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p196-b1-b2-ee578070/results/adopted-world-8-2-progress-213-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `18fe08991e9f53de44ca0231e71306101d3db6846d1f58d05bde74851aa76c7a`;
  exactly 110,764 bytes and 3,450 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 213 }`; exactly 161,449
  frames; mechanical state `(world=7, level=1, progress=213,
  player_y_bucket=11, player_engine_state=5, dead=false, flag_active=true)`;
  frozen key `(7,1,213,11,5,state_fingerprint=47)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `6f1b96d92cfc62464fde03fc725a55334b1000a260f462e1cd63d067486e6e62`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `38b772afb3fd1cb73f344fca6bf79dd48eda663aebfc2e16418812793f17d367`.
- Final opaque `ButtonChord { buttons: 2, hold_frames: 103 }`. Registered
  source probe: mask `0x00` survives all 45 frames; total source-probe work 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p213-normal-endpoint-transition-harvest-v1`;
  label SHA-256
  `cd6003a9bc5e8c3eb6530dc4330d49794a07dde0a5d9f85614f2f15e7f6ff1f3`;
  little-endian first-eight-byte master seed `4507081491473457357`.

The standalone binary is `smb-w8-2-p213-normal-endpoint-transition-harvest`;
its positional arguments are `<input.json> <create-new-output.jsonl>`, and it
reads the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB
and 16 MiB using maximum plus one.

Before recipe generation or lane execution, replay the source once from
gameplay genesis and verify every source fact above. From a fresh restore,
reproduce the registered mask-`0x00` 45-frame survival, then restore and
re-hash the source snapshot. Record the sealed trace framing and trace hash.
Any mismatch is integrity **STOP**.

## Frozen occurrence-marginal recipes

There are twelve independent lanes `l=0..11`, each with 128 draws `d=0..127`.
After the source baseline passes, derive:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("p213-b1-transition-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("p213-b1-transition-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3450

selector_digest = SHA-256(
  lane_seed_u64_le || ASCII("p213-b1-transition-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete source `ButtonChord` occurrence at `source_index`. There is
no retry, filter, deduplication, semantic inspection, state association,
empirical update, or outcome feedback.

Serialize the lane-major, draw-minor 1,536-element vector
`(l_u64,d_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its byte length and SHA-256. For each lane,
serialize and hash one bare `Vec` in draw order whose exact projection element
is `(d_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `l` and
any wrapper. Before lanes, require all twelve exact projection byte vectors to
be pairwise distinct; collision is integrity STOP with no retry. Freeze all
recipes before live work. Do not deduplicate recipes or cross-lane outcomes;
normal per-lane archive duplicate detection remains unchanged.

## Normal B1 lanes

Each lane starts a fresh archive containing only validated p213 as `id=0`,
`parent_id=None`, `created_execution=0`, directly inserted without an origin
probe. Require one active entry. Every lane uses action limit 4,096; archive
limit 129; `Frozen` key; `ProbeAtAdmission45` masks `[0x00,0x01,0x81]`;
`FewestActions`; existing `ConcentratedRecency` selection/productivity
accounting; and no waypoint, snapback, pinned window, phrase, sibling burst,
compaction, or empirical chord update. No lineage may exceed
`3450+128=3578` actions.

For draw `d`, initialize fresh `StdRand(selector_seed[d])`, call real
`Archive::select_parent` once, restore and verify it, build
`parent_input + chord[d]`, execute the one action, and process the normal
endpoint through snapshot, ordered probe, restore, duplicate, and admission.
Sequence and `created_execution` are `d+1`. Call `record_selection` and
`record_selection_outcome` once on the selected parent. Productive means the
candidate newly allocates; cost is its action plus probe work.

Any non-Ok `ExitKind`, worker error, or emulator error is integrity STOP.
Death with `ExitKind::Ok` ends only that draw; the next draw selects normally.

## Work, report, and adoption

Use exactly twelve persistent workers, one lane per ordinal, and return ordinal
plus inner success/error. The coordinator buffers replies, consumes them
ascending, and is the sole writer. Any ordinal, restore, arithmetic,
accounting, or report mismatch is integrity STOP.

The deterministic hard bound is 184,320 scheduled action frames; 207,360 live
probe frames; 161,449 source replay frames; 45 source-probe frames; and 4,693
setup frames from thirteen targets at 361 each. Total hard bound is
**557,867 frames**. Require exactly 1,536 scheduled and executed candidates and
1,536 selections. Record all work components, active counts, and maximum
lineage with checked reconciliation. Expected `msr1` time is 3–6 minutes;
allow 10 operationally. Wall time is not recorded or a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, lanes
ascending, adoption classification, summary. Per candidate record lane, draw,
recipe, selector, parent/input/snapshot, candidate input, endpoint
state/hashes, probe, admission, active set, accounting, and work. The header
binds preregistration, source, ROM, executable, runner sources,
recipe/projections, trace, and config hashes. `body_sha256` covers bytes through
the last pre-summary LF; after summary and LF, flush, sync, and print whole-file
SHA-256. No host path, timestamp, or wall-clock field is permitted.

Adoption-eligible entries are final-active, newly allocated, live,
probe-surviving normal endpoints. Exclude source, inactive entries, deaths,
refusals/rejections, and duplicates. Rank one global champion by greatest full
target-provided lexicographic watermark; fewest actions; ascending raw semantic
input SHA-256; ascending lane; then entry id.

Verdict is **ADOPT** iff that champion is strictly greater than source full
watermark `(7,1,213)`. A lexicographically later level or world is eligible on
the same rule; no transition-specific predicate is added. Embed the exact
input and complete lane/draw/lineage/state/hash/work evidence. It is the sole
authorized next source. Otherwise verdict is **STOP** and nothing is
adoptable; the next structural test may only be a separately preregistered
midpoint-compaction canary. Any integrity mismatch authorizes nothing. There
is one registered run, no routine replay audit, rerun, or post-hoc candidate
choice.
