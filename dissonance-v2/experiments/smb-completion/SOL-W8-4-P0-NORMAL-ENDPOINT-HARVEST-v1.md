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
