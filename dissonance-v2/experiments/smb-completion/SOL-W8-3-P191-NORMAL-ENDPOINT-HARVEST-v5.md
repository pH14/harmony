<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-3 p191 normal endpoint harvest v5

Status: preregistered after the p191 result commit and before implementation,
recipes, ROM loading, or live emulation.

## Question and terminal boundary

The ordinary full-source B1 policy advanced p54 to p87 to p187, then only to
p191. This final full-headroom replication asks whether the same generic policy
can cross to a later level before structural work is justified. It runs twelve
fresh independent lanes of 512 one-action draws. If the verdict is ADOPT and
the champion `(world,level)` is lexicographically later than `(7,2)`, ordinary
endpoint continuation remains justified. STOP or a same-level ADOPT requires
`CHANGE_STRUCTURE`; a same-level adopted input remains the sole valid source
for that separately preregistered structural test. There is no second p191
normal replication.

No route, coordinate, button, duration, transition action, waypoint, semantic
filter, state/action association, structural treatment, or operator prior is
used. The runner reads only the exact p191 input and ROM, never any prior
report, archive, result, candidate, snapshot, recipe, or manifest.

## Frozen source and seed

- Code base/result commit: `afcb737c67d060ffcc2d50b6b41a95e9fa255bfa`.
- Authorizing p187 preregistration `e80b6971`, implementation `cae526e4`,
  result `afcb737c`, report SHA-256
  `7939f1fbe24a16241fde1ab95d637839f9a2f3aa29d2365374ba18f9d0c9b3ad`.
- Source:
  `/root/harmony-smb-sol-w8-3-p187-harvest-e80b6971/results/adopted-world-8-3-progress-191-input.json`;
  compact/semantic SHA-256
  `db39971b3ee10119d0d14224f8fc4fea79ac65c5a2f14b7cfc6785a57df08836`;
  112,798 bytes; 3,513 actions.
- Alive Ok replay maximum/endpoint `(7,2,191)` at exactly 164,814 frames;
  mechanical `(7,2,191,y=9,engine=8,dead=false,flag=false)`; frozen key
  `(7,2,191,9,8,state_fingerprint=30)`.
- Milestones `(195,true,true,true)` in field order
  `(max_1_1_scroll_bucket,reached_1_1_flag,reached_1_2,reached_onward)`.
- WRAM SHA-256
  `9e64d29a26b9570c2d6129f1cd0f80a3139b5d15fe3da5b45bbf453212ff1e5f`;
  snapshot SHA-256
  `73729a4c2a49ea44b138a2ff66b63a49bdb53c9e363c2ac97336d45543295dc9`.
- Final opaque chord `{buttons:1,hold_frames:108}`. Mask `0x00` survives the
  exact 45-frame source probe. ROM SHA-256 is
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-3-p191-normal-endpoint-harvest-v5`; SHA-256
  `dd8a1dc4726a198c134c073178f7ced81bca3931e55cc837dc68c437407b1b52`;
  first-eight little-endian master seed `10095217080876763869`.

Binary `smb-w8-3-p191-normal-endpoint-harvest-v5` takes
`<input.json> <create-new-output.jsonl>` and ROM only through
`HARMONY_SMB_ROM`. Bound reads at 2 MiB and 16 MiB. Replay and verify every
fact, then source-probe, restore, and re-hash before recipes or workers. Record
the deterministic trace hash. Any mismatch is integrity STOP.

## Frozen recipes and normal lanes

For `l=0..11`, `d=0..511`:

```text
lane_seed = first8_le(SHA256(master_le || "w8-3-p191-v5-lane" || l_le))
source_index = first8_le(SHA256(lane_seed_le || "w8-3-p191-v5-action" || d_le)) mod 3513
selector_seed = first8_le(SHA256(lane_seed_le || "w8-3-p191-v5-parent" || d_le))
```

Copy the complete source chord occurrence. No retry/filter/dedup/inspection or
outcome feedback. Hash exact serde-JSON lane-major tuples
`(l,d,index,ButtonChord,selector_seed)` and each bare lane projection
`Vec<(d,index,ButtonChord,selector_seed)>`. Require all twelve projection byte
vectors pairwise distinct before workers; collision is integrity STOP without
retry.

Each lane starts a fresh archive with trusted source id0 only. Use action limit
4096, archive513, Frozen key, ProbeAtAdmission45 masks `[00,01,81]`,
FewestActions, real ConcentratedRecency accounting, and absent waypoint,
snapback, pin, phrase, burst, compaction, and empirical update. Maximum lineage
is `3513+512=4025`.

For each draw use fresh `StdRand(selector_seed)`, one real selection, exact
parent restore, one action, normal snapshot/probe/restore/duplicate/admission,
then exactly one selection and outcome accounting call. Productive means new
allocation. Ok-death ends only its draw; non-Ok/worker/emulator error is
integrity STOP.

## Work, report, and verdict

Exactly twelve persistent workers and coordinator-ordered replies/reporting.
Require 6,144 candidates and selections. Hard caps: action 737,280; candidate
probe 829,440; source replay 164,814; source probe 45; thirteen setups 4,693;
**1,736,272 total**. Reconcile checked counters; wall time is not authority.

Create-new NDJSON is header, baseline, recipes, lanes ascending, adoption,
summary. Bind all provenance/source/ROM/executable/source/config/recipe/trace
hashes and exact body/whole-file framing. No host paths or time fields.

Eligible champions are final-active, newly allocated, alive, probe-surviving
ordinary endpoints. Rank by full watermark descending, actions ascending,
semantic input SHA ascending, lane, id. **ADOPT** iff strictly greater than
`(7,2,191)`; later levels remain eligible. Embed the exact sole next-source
input/evidence. Otherwise **STOP**. One run only; no rerun, relaxation, routine
replay audit, or post-hoc candidate choice. Apply the terminal boundary above.
