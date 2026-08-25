<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-4 p0 normal endpoint harvest v1

Status: preregistered after the World 8-4 transition result and before runner
sealing, recipes, ROM loading, or live emulation.

## Purpose

The registered final World 8-3 replication crossed to a live, probe-surviving
World 8-4 endpoint. This run applies the proven ordinary full-source B1 policy
to the exact transition state: twelve fresh independent lanes, 512 one-action
draws each. It introduces no final-level, route, coordinate, button, duration,
state/action, waypoint, transition, or semantic prior. It reads only the exact
source input and ROM; every prior report/artifact is runtime-forbidden.

## Frozen source and seed

- Code/result commit `3be2e5e00d232ec11d500ceb96a8831c80e2257a`.
- Authorizing p191 preregistration `52eadc8f`, implementation `79c184c1`,
  result `3be2e5e0`, report SHA-256
  `3feffad9255911dffc0278aaffbe5c45801db8b1aff1c03ff45cdb95f78bc7e3`.
- Source:
  `/root/harmony-smb-sol-w8-3-p191-harvest-52eadc8f/results/adopted-world-8-4-progress-0-input.json`;
  compact/semantic SHA-256
  `59f00e2dda00c730cda3c44e441fd94c65ee28c641be10e69be00c522522b706`;
  113,193 bytes; 3,525 actions.
- Alive Ok replay maximum/endpoint full watermark `(7,3,0)` at 165,794 frames;
  mechanical `(7,3,0,y=0,engine=0,dead=false,flag=false)`; frozen key
  `(7,3,0,0,0,state_fingerprint=9)`.
- Milestones `(195,true,true,true)` in field order
  `(max_1_1_scroll_bucket,reached_1_1_flag,reached_1_2,reached_onward)`.
- WRAM SHA-256
  `495908631d94d76765a350ee6b17b40dfc0a02614090eee7c8c199f7cc5e251c`;
  snapshot SHA-256
  `620d9ee95be67da58fe943e44b9e94895cc1b4afc98243ad2e8a9a296364abf8`.
- Final opaque chord `{buttons:32,hold_frames:99}`; mask `0x00` survives the
  exact 45-frame source probe. ROM SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-4-p0-normal-endpoint-harvest-v1`; SHA-256
  `6dc2f01dc328a7febeb108a53d041e890967040b15310420c6fa523a80f00a62`;
  first-eight little-endian master seed `18349680025230426733`.

Binary `smb-w8-4-p0-normal-endpoint-harvest-v1` takes
`<input.json> <create-new-output.jsonl>` and ROM only via `HARMONY_SMB_ROM`.
Bound source/ROM reads at 2/16 MiB. Before recipes/workers, replay from genesis,
verify every fact, run the source probe, restore/re-hash, and record the sealed
trace. Mismatch is integrity STOP.

## Frozen recipes and execution

For `l=0..11`, `d=0..511`:

```text
lane_seed = first8_le(SHA256(master_le || "w8-4-p0-v1-lane" || l_le))
source_index = first8_le(SHA256(lane_seed_le || "w8-4-p0-v1-action" || d_le)) mod 3525
selector_seed = first8_le(SHA256(lane_seed_le || "w8-4-p0-v1-parent" || d_le))
```

Copy the complete opaque source chord occurrence. No retry, filter,
deduplication, semantic inspection, state association, empirical update, or
outcome feedback. Hash exact serde-JSON lane-major recipe tuples and bare
draw-ordered lane projections. Require all twelve projection byte vectors
pairwise distinct before workers, with collision causing integrity STOP and no
retry.

Each lane starts with trusted source id0 only. Use action limit4096,
archive513, Frozen key, ProbeAtAdmission45 `[00,01,81]`, FewestActions, real
ConcentratedRecency accounting, and absent waypoint/snapback/pin/phrase/burst/
compaction/empirical update. Maximum lineage is `3525+512=4037`.

Each draw uses fresh `StdRand(selector_seed)`, one real parent selection and
verified restore, one action, ordinary snapshot/probe/restore/duplicate/
admission, and exactly one selection/outcome accounting call. Productive means
new allocation. Ok-death consumes only that draw; non-Ok or worker/emulator
error is integrity STOP.

## Work, report, and decision

Exactly twelve persistent workers; coordinator buffers and consumes ascending
and is sole writer. Require 6,144 candidates/selections. Hard caps: action
737,280; candidate probes829,440; replay165,794; source probe45; setup4,693;
**1,737,252 total**. Checked reconciliation only; wall time is not authority.

Create-new NDJSON order: header, baseline, recipes, lanes, adoption, summary.
Bind all provenance/source/ROM/executable/source/config/recipe/trace hashes,
canonical errors, body bytes, and whole-file bytes. No host paths/timestamps.

Eligible champions are final-active, newly allocated, alive, probe-surviving
ordinary endpoints. Rank full watermark descending, actions ascending, input
SHA ascending, lane, id. **ADOPT** iff strictly greater than `(7,3,0)`; embed
the exact sole next-source input/evidence. Otherwise **STOP**. This is one run
with no rerun, relaxation, routine replay audit, or post-hoc candidate choice.
Any observed terminal/credits-like state is diagnostic until a separately
frozen mechanical completion predicate and artifact-only replay confirm it.

## Registered result

The registered run completed successfully under implementation `597ea67f`
and executable SHA-256
`f24426757a596e4f2bdd31128956bee69c0f372d398e2d0c94961fa2cd2c71b8`.
The 797,872,839-byte report at
`/root/harmony-smb-sol-w8-4-p0-harvest-4f0e7549/results/w8-4-p0-normal-endpoint-harvest-12x512.jsonl`
has whole-file SHA-256
`255f9b430841303a4e5d9c9d6eb9820c1887ba9c7b3c3f5192d53c2c1eb87e59`
and body SHA-256
`9f0791def420da48a256905909f02aa6ae38a94294764bb5074877754f1230c4`.
All 6,144 candidates/selections executed with empty stderr. Work was 4,693
setup, 165,794 replay, 45 source probe, 271,327 action, and 235,795 candidate
probe frames: 677,654 total.

The verdict is **ADOPT**. Lane 9 entry 363 at draw 435, parent 343, has
lineage `[0,1,3,6,8,29,52,56,57,60,64,65,70,73,74,75,82,123,137,222,252,272,298,343,363]`.
Its 3,549-action input is alive and probe-surviving and advances World 8-4
from progress 0 to **progress 61** at frame 167,136. Mechanical state is
`(7,3,61,y=11,engine=8,dead=false,flag=false)`. Input, WRAM, and snapshot
SHA-256 values are respectively
`15572c6ea86e749d89995a74ee725bf76a5da500b14efa508635d3e2f664da4c`,
`fae8453cb375f25a913d34d2c3aaf8d9d5d5fd109269eaf845d2c0a6cec9781e`,
and `62761b7a01aa2d942ea44da20b814e657fb20ff4036f918738c2c8d980e914be`.
The sole next-source file is
`/root/harmony-smb-sol-w8-4-p0-harvest-4f0e7549/results/adopted-world-8-4-progress-61-input.json`,
113,972 bytes with the same input SHA-256.
