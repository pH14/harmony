<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol C119 normal-endpoint frontier harvest v1

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Question and boundary

Run one small, adoptable frontier harvest using only the existing normal SMB
search path from exact C119. This is not a policy comparison. The immutable
C119 endpoint is the control/comparator; every live draw is spent on ordinary
endpoint search.

No prior canary report is an input. The binary must not read any prior canary
report, stream, candidate input, snapshot, recipe, selected duration, action,
maximum, or result. In particular, no progress-237 prefix or later paired-
canary candidate may enter initialization, proposal, ranking, or adoption.

Proposal logic treats buttons as opaque and never decodes route, coordinates,
room identity, world, level, progress, or operator goals. The source action
marginal, target-defined archive key, existing selector, and target-provided
`SmbProgressWatermark::Ord` are generic retained-artifact/domain adapters, not
operator priors.

## Frozen source and seed

- Code base before this experiment: `2e2ec0bf`.
- C119 archive SHA-256:
  `d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81`.
- C119 stream SHA-256:
  `ab869286a526dab104f7846ae0313745de7087e3733e99016218defb42e90201`.
- Selected entry `48076`, parent `29805`, created at execution `49709`; 3,297
  actions; compact source byte SHA-256
  `5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159`;
  semantic `SmbInput` SHA-256
  `584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1`.
- Verified source: alive `ExitKind::Ok`, maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 0, progress: 236 }`, exactly
  155,148 frames; endpoint mechanical state
  `(world=7, level=0, progress=236, player_y_bucket=7,
  player_engine_state=8, dead=false, flag_active=false)`.
- Source trace SHA-256:
  `9245f6d42f684a1fcd0a33a762519a51270d1ece2b695ea5a575d83ff64149a1`;
  raw-WRAM SHA-256:
  `936ac08d4c48a2968bec111324fd7ed28628ea89b35baa049b1b5abfffc896ea`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `107bab5a4691ca0e43586b3c95849031782d40f2a3013856161ae4f1d997ae66`.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-c119-normal-endpoint-frontier-harvest-v1`;
  label SHA-256
  `242760c7685790c3abe44aeea30523b8a5a3af7a07d7fdbdff6c6d0145e706f1`;
  little-endian first-eight-byte master seed `14091859341575464740`.

The standalone binary is `smb-endpoint-frontier-harvest`; its positional
arguments are `<input.json> <create-new-output.jsonl>`, and it reads the ROM
only from `HARMONY_SMB_ROM`. The bounded input is decoded directly as
`SmbInput`; the binary reads no archive, stream, manifest, or prior result.
Source and ROM reads are capped at 2 MiB and 16 MiB using maximum plus one.

Replay the source once from gameplay genesis and validate every derivable
identity above. Use the sealed trace framing: SHA-256 domain
`"smb-trace-canary-v1\0trace\0"`, length-framed canonical JSON for the genesis
observation, then each zero-based action index as little-endian `u64` followed
by length-framed canonical JSON for the action and its ordered observations.

## Frozen recipes

There are twelve independent lanes `l = 0..11`, each with 256 serial draws
`d = 0..255`. Before loading the ROM, derive:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3297

selector_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete opaque source `ButtonChord` at `source_index`. There is no
retry, filter, deduplication, semantic inspection, state association, or
outcome feedback. Serialize the lane-major, draw-minor ordered vector
`(l_u64, d_u64, source_index_u64, ButtonChord, selector_seed_u64)` with
`serde_json::to_vec` and record its SHA-256 before ROM loading.

## Lanes and one draw

Each lane starts a new archive containing only the validated C119 source as
`id=0`, `parent_id=None`, `created_execution=0`. Insert it directly from the
validated input/key/milestones/snapshot with no origin probe or added
emulation, and require exactly one active entry.

Every lane uses:

- action limit 4,096 and archive limit 257;
- `Frozen` key;
- `ProbeAtAdmission45` with masks `[0x00, 0x01, 0x81]` in that order;
- `FewestActions` replacement;
- existing `ConcentratedRecency` selection/productivity accounting; and
- absent waypoint, snapback, pinned window, and empirical chord update.

At each draw, initialize a fresh `libafl_bolts::rands::StdRand` with the frozen
`selector_seed`, call real `Archive::select_parent` once with action limit
4,096, and record its complete selector draw. Restore and verify the selected
snapshot, apply the one frozen full action, and record start/endpoint
observations, raw state and hashes, input identity, milestones, work, death,
and failure.

If the endpoint is alive with `ExitKind::Ok`, snapshot it, run the frozen
viability probe, restore it exactly, and offer it to the real archive as the
selected parent's child with `created_execution=d+1`. Record duplicate,
probe-refused, rejected, displaced, and newly retained outcomes. Productivity
is true only for a newly allocated retained endpoint; selector cost is the
draw's exact action plus probe work. Death is an outcome; emulator failure or
non-restoration is an integrity STOP.

After that endpoint outcome, call
`Archive::record_selection(parent_id, &selector_draw)` and then
`Archive::record_selection_outcome(parent_id, productive,
realized_draw_frames)` exactly once and in that order on every draw, including
death, probe refusal, duplicate, and rejection. A replacement which allocates
a new entry is productive; returning an existing duplicate id is not. This
accounting completes before the next draw's `select_parent` call.

Do not reconstruct, probe, or admit an interior observer event. Only ordinary
full-action endpoints may enter the archive or become adoptable.

Use exactly twelve persistent workers, one lane per worker. Each worker
returns its fixed lane ordinal and inner success/error. The coordinator
buffers and consumes replies in ascending lane order and is the only report
writer. Missing, duplicate, or wrong-worker replies are integrity STOPs.

## Work and evidence

The deterministic hard bound is:

- 3,072 full actions: 368,640 frames;
- endpoint probes: 414,720 frames;
- one source replay: 155,148 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 943,201 frames. Record every component and reconcile target
work deltas with their sums using checked arithmetic. Crossing any component
or total bound is an integrity STOP. Expected `msr1` live time is 6–9 minutes;
allow 12 minutes operationally. Wall time is neither recorded nor a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, lanes
in ascending order, adoption classification, then summary. Record enough
per-draw archive, selector, input, snapshot, probe, lineage, active-set, and
work evidence to recompute every final value. The header binds the preregistered
source/ROM, current executable, runner sources, recipe, and config hashes.
`body_sha256` covers exact UTF-8 NDJSON bytes through the last pre-summary LF;
after summary and LF, flush, sync, and print the whole-file SHA-256. The report
contains no host path, timestamp, or wall-clock field.

## Frozen adoption decision

Eligible entries are only final-active entries newly allocated through normal
ordinary endpoint admission in this run. Entry zero, inactive entries,
transient observations, interior prefixes, duplicates, probe refusals, and
rejections are ineligible.

Choose at most one champion by this total order:

1. greatest target-provided `SmbProgressWatermark`;
2. fewest actions;
3. ascending raw semantic-input SHA-256 bytes;
4. ascending lane; then
5. ascending entry id.

Verdict **ADOPT** iff that champion exists, is live and probe-surviving by its
recorded normal admission, and its endpoint watermark is strictly greater than
the registered C119 watermark `(7, 0, 236)`. Embed its exact `SmbInput`, lane,
id, parent lineage, endpoint evidence, raw-WRAM/input/snapshot hashes, and work
in the report. This exact input—and no other result of the run—is authorized
as the sole source for a separately preregistered next campaign. Before that
campaign makes proposals, replay it once from gameplay genesis and require
exact agreement with all recorded adoption evidence.

If no champion passes, verdict **STOP** and no candidate is adoptable. Any
identity, recipe, worker, restore, evidence, hash, or work mismatch is an
integrity STOP and authorizes nothing. There is one registered live run, no
routine replay audit, no automatic rerun, no post-hoc ranking, and no adoption
from any prior canary.

## Result

The one registered run completed successfully on `msr1` with verdict
**ADOPT**. The temporary implementation was sealed at commit `fe5e2c75`, with
binary-source SHA-256
`758211b5ea937af286df5e3529737de3547cbeb95758a25efe9e1433d0fe1aa7`,
module-source SHA-256
`2c8b45b161a19edfe800db40c0013d75a2f36f281bdcbac3fc87cc2288a6df54`,
and release-executable SHA-256
`8324c837bc7292d8df54f74cd12040f0492ce44f8a179b1ee53d48cd60ae1ed3`.
The frozen recipe SHA-256 was
`19fe77bdd38e4516454e3ccc4467790d280428183a44bb1395d4c788bcb09735`.

The canonical 17-line report is stored at
`/root/harmony-smb-sol-endpoint-harvest-e3ca732b/results/c119-normal-endpoint-harvest-12x256.jsonl`.
It is 51,802,752 bytes with body SHA-256
`91e20f91c27ab79eb723b1c289d78888e2650b15bc1a1617246d21d897b1357e`
and whole-file SHA-256
`a3b744d6b61ea573fd9ac12205b38cf7d32be083d6e0b09d0577a16b565d2ce9`.
Standard error was empty. Realized work was 389,711 frames: 155,148 source
replay, 4,693 setup, 114,819 action, and 115,051 probe frames.

Final lane watermarks, in lane order, were
`[255, 270, 240, 270, 245, 253, 270, 253, 270, 245, 244, 253]` in world 8-1.
The registered total order selected lane 8 entry 186, with lineage
`[0, 1, 38, 43, 47, 57, 59, 68, 73, 75, 80, 120, 186]`. It is a live,
probe-surviving, normally retained endpoint at progress 270 after 3,309
actions and 155,855 absolute frames. Its semantic input SHA-256 is
`9a71c1ab63f1f16eb9f34b38f66047e3cbfa8d0623a1219eda839393dab01921`,
raw-WRAM SHA-256 is
`37aca14a7115b7f9cc700e8754b1b1243f2635c205d4696242782aaa9e354908`,
and snapshot SHA-256 is
`382434bfb1bc83c70c2c074272a0e641238d48e4c3a5a9aa9efc0649e9d9877d`.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-endpoint-harvest-e3ca732b/results/adopted-progress-270-input.json`;
its compact-file SHA-256 is the same semantic input hash above. This advances
the retained C119 frontier from 236 to 270. Its required from-genesis
verification will be the baseline phase of the separately preregistered next
continuation, before that continuation materializes any live proposal.
