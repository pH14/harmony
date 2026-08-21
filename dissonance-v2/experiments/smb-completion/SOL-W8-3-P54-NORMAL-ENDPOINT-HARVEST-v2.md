<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-3 p54 normal endpoint harvest v2

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Purpose and boundary

The registered short B1 harvest crossed from World 8-2 to a live,
probe-surviving normal endpoint at World 8-3 progress 54. This v2 continuation
changes only the frozen budget: twelve independent lanes receive 512 normal
one-action draws each. It retains the registered full-source opaque occurrence
marginal and ordinary B1 archive machinery.

There is no button-semantic, duration-selection, route, coordinate, waypoint,
state/action-association, transition-specific action, structural treatment, or
operator prior. The binary may read only the exact p54 `SmbInput` and ROM named
below. It must not read the p213 harvest report or any earlier report, archive,
result, candidate, snapshot, stream, manifest, or recipe.

## Frozen source and seed

- Code base before this experiment and registered p54 result commit:
  `50c7319dec93c4f7725f870dea8e7ba90eaf96c3`.
- Authorizing p213 harvest preregistration: `646a14cc`; sealed implementation:
  `4a09d824`; registered result commit: `50c7319d`; registered report SHA-256:
  `d38e331e8815e9c0d20adf4a1e0becbf440467f9b957ab50958cf8370e70d5fe`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p213-transition-646a14cc/results/adopted-world-8-3-progress-54-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `1544342bd57911b92fd4beefd2eebc9e7db15fa0077c37338d9b7c12048e8d99`;
  exactly 111,392 bytes and 3,469 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 2, progress: 54 }`; exactly 163,227
  frames; mechanical state `(world=7, level=2, progress=54,
  player_y_bucket=11, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,2,54,11,8,state_fingerprint=30)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `1e6b30b702f29098605b16abed7ebcc9a618b7525009c192b5f19b86c9cdf9a4`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `e42a2f69123518ab95813429fbc312553b4af205ff0053915de1be52a6125189`.
- Final opaque `ButtonChord { buttons: 16, hold_frames: 112 }`. Registered
  source probe: mask `0x00` survives all 45 frames; total source-probe work 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-3-p54-normal-endpoint-harvest-v2`; label SHA-256
  `3a55df6e0d24e844d4cb3614fc76a1d90899fbe490e0349c10c1a835786823cc`;
  little-endian first-eight-byte master seed `4965258229289276730`.

The standalone binary is `smb-w8-3-p54-normal-endpoint-harvest-v2`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipe generation or lane execution, replay the source once from
gameplay genesis and verify every source fact above. From a fresh restore,
reproduce the registered mask-`0x00` 45-frame survival, then restore and
re-hash the source snapshot. Record the sealed trace framing and trace hash.
Any mismatch is integrity **STOP**.

## Fresh frozen occurrence recipes

There are twelve independent lanes `l=0..11`, each with 512 draws `d=0..511`.
After the source baseline passes, derive:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("w8-3-p54-v2-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p54-v2-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3469

selector_digest = SHA-256(
  lane_seed_u64_le || ASCII("w8-3-p54-v2-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete source `ButtonChord` occurrence at `source_index`. There is
no retry, filter, deduplication, semantic inspection, state association,
empirical update, or outcome feedback.

Serialize the lane-major, draw-minor 6,144-element vector
`(l_u64,d_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its byte length and SHA-256. For each lane,
serialize and hash one bare `Vec` in draw order whose exact projection element
is `(d_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `l` and
any wrapper. Before lanes, require all twelve exact projection byte vectors to
be pairwise distinct; collision is integrity STOP with no retry. Freeze all
recipes before live work. Do not deduplicate recipes or cross-lane outcomes;
normal per-lane archive duplicate detection remains unchanged.

## Normal B1 lanes

Each lane starts a fresh archive containing only validated p54 as `id=0`,
`parent_id=None`, `created_execution=0`, directly inserted without an origin
probe. Require one active entry. Every lane uses action limit 4,096; archive
limit 513; `Frozen` key; `ProbeAtAdmission45` masks `[0x00,0x01,0x81]`;
`FewestActions`; existing `ConcentratedRecency` selection/productivity
accounting; and no waypoint, snapback, pinned window, phrase, sibling burst,
compaction, or empirical chord update. No lineage may exceed
`3469+512=3981` actions.

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

The deterministic hard bound is 737,280 scheduled action frames; 829,440 live
probe frames; 163,227 source replay frames; 45 source-probe frames; and 4,693
setup frames from thirteen targets at 361 each. Total hard bound is
**1,734,685 frames**. Require exactly 6,144 scheduled and executed candidates
and 6,144 selections. Record all work components, active counts, and maximum
lineage with checked reconciliation. Expected `msr1` time is 8–14 minutes;
allow 20 operationally. Wall time is not recorded or a stop.

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
watermark `(7,2,54)`. A lexicographically later level or world is eligible on
the same rule; no transition-specific predicate is added. Embed the exact
input and complete lane/draw/lineage/state/hash/work evidence. It is the sole
authorized next source. Otherwise verdict is **STOP** and nothing is
adoptable. Any integrity mismatch authorizes nothing. There is one registered
run, no routine replay audit, rerun, or post-hoc candidate choice.
