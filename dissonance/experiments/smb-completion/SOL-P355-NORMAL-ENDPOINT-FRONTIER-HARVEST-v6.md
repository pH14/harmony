<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol p355 normal-endpoint frontier harvest v6

Status: preregistered before implementation seal, recipe materialization, ROM
loading, or live candidate emulation.

## Question and boundary

Continue the existing normal SMB endpoint search from the sole registered p355
adoption. This is one adoptable harvest, not a policy comparison. The immutable
p355 endpoint is the comparator and every live draw is ordinary endpoint search.

The binary may read only the exact adopted `SmbInput` named below and the ROM.
It must not read the v5 report, any archive, stream, manifest, result, candidate,
snapshot, or recipe. No unregistered v5 candidate may enter initialization,
proposal, ranking, or adoption.

Proposal logic treats buttons as opaque and never decodes route, coordinates,
room identity, world, level, progress, or operator goals. The validated source
action marginal, target-defined archive key, existing selector, and
target-provided `SmbProgressWatermark::Ord` are generic retained-artifact/domain
adapters, not operator priors.

## Frozen source and seed

- Code base before this experiment: `9ccfced3`.
- Authorizing v5 preregistration: `5b8fa319`; sealed implementation:
  `4dcb765c`; registered result: `9ccfced3`.
- Exact source file:
  `/root/harmony-smb-sol-p351-harvest-5b8fa319/results/adopted-progress-355-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `9103222be5df58c7dbb46bd988024f8136b0a28dde0cec13d769393298faf7cd`;
  exactly 3,347 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 0, progress: 355 }`; exactly 157,182
  frames; mechanical state `(world=7, level=0, progress=355,
  player_y_bucket=7, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,0,355,7,8,state_fingerprint=37)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `650839c118ea2c06e91c47391466dfaa0606718756c96bf50ac488e7b67401a7`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `e6c4c9215d9572cd1741cf655e0e6377e948c7b042ea7bde8d9939b38cba8bd4`.
- Registered final action: opaque `ButtonChord { buttons: 131,
  hold_frames: 9 }`. Registered source probe: mask `0x00`, 45 frames,
  alive and survived.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-p355-normal-endpoint-frontier-harvest-v6`;
  label SHA-256
  `14f04b6d63e473e6ccc3390cec3a8f08c48800bf60f69d9d2dce7716968e6276`;
  little-endian first-eight-byte master seed `16605867366731739156`.

The standalone binary is `smb-p355-endpoint-frontier-harvest`; positional
arguments are `<input.json> <create-new-output.jsonl>`, and the ROM is read only
from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and 16 MiB using
maximum plus one.

Before materializing a proposal or initializing a lane, replay the source once
from gameplay genesis. Verify every source identity and replay-derived fact
above, including the exact endpoint raw WRAM, snapshot, maximum, key,
milestones, final action, and mechanics. Run the registered 45-frame mask
`0x00` source probe, require survival, and restore and re-hash the snapshot.
Use the sealed trace framing from v5 and record the newly computed trace hash.
Any mismatch is integrity **STOP** before recipes or live draws.

## Frozen recipes

After the baseline passes, materialize twelve independent lanes `l = 0..11`,
each with 700 serial draws `d = 0..699`:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3347

selector_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete opaque source `ButtonChord` at `source_index`. There is no
retry, filter, deduplication, semantic inspection, state association, or
outcome feedback. Serialize the lane-major, draw-minor ordered vector
`(l_u64, d_u64, source_index_u64, ButtonChord, selector_seed_u64)` with
`serde_json::to_vec` and record its SHA-256 before lane initialization.

## Lanes and one draw

Each lane starts a new archive containing only the validated p355 source as
`id=0`, `parent_id=None`, `created_execution=0`. Insert it directly from the
validated input/key/milestones/snapshot with no origin probe or added
emulation, and require exactly one active entry.

Every lane uses action limit 4,096; archive limit 701; `Frozen` key;
`ProbeAtAdmission45` masks `[0x00, 0x01, 0x81]`; `FewestActions` replacement;
existing `ConcentratedRecency` selection/productivity accounting; and no
waypoint, snapback, pinned window, or empirical chord update.

For every draw, initialize a fresh `libafl_bolts::rands::StdRand` with the
frozen `selector_seed`; call real `Archive::select_parent` once; record its
complete selector draw; restore and verify the selected snapshot; and apply
the one frozen full action. Record start/endpoint observations, raw state and
hashes, input identity, milestones, work, death, and failure.

If the endpoint is alive with `ExitKind::Ok`, snapshot it, run the frozen
viability probe, restore it exactly, and offer it to the real archive as the
selected parent's child with `created_execution=d+1`. Record duplicate,
probe-refused, rejected, displaced, and newly retained outcomes. Productivity
is true only for a newly allocated retained endpoint; selector cost is the
draw's exact action plus probe work.

After the endpoint outcome, call
`Archive::record_selection(parent_id, &selector_draw)` and then
`Archive::record_selection_outcome(parent_id, productive,
realized_draw_frames)` exactly once and in that order on every draw. Do not
reconstruct, probe, or admit interior observer events. Only ordinary
full-action endpoints may enter the archive or become adoptable.

Use twelve persistent workers, one lane each. The coordinator buffers replies
and consumes them in ascending lane order and is the only report writer.
Worker, restore, emulator, arithmetic, or report failure is integrity STOP.

The source has 3,347 actions and each lane has only 700 draws, so no lineage
may exceed 4,047 actions. Record each lane's maximum lineage length and treat
any value above 4,047 as an integrity STOP; the 4,096 action limit cannot bind
an otherwise registered proposal.

## Work, evidence, and verdict

The deterministic hard bound is:

- 8,400 full actions: 1,008,000 frames;
- live endpoint probes: 1,134,000 frames;
- one source replay: 157,182 frames;
- one source evidence probe: 45 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 2,303,920 frames. Record and reconcile every component with
checked arithmetic. Crossing any component or total bound is integrity STOP.
Expected `msr1` live time is 16–25 minutes; allow 32 minutes operationally.
Wall time is neither recorded nor a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, lanes
ascending, adoption classification, summary. Record enough per-draw archive,
selector, input, snapshot, probe, lineage, active-set, and work evidence to
recompute every final value. The header binds this preregistration, source,
ROM, executable, runner sources, recipe, and config hashes. `body_sha256`
covers exact bytes through the final pre-summary LF; after summary and LF,
flush, sync, and print the whole-file SHA-256. No host path, timestamp, or
wall-clock field is permitted.

Eligible entries are only final-active entries newly allocated through normal
ordinary endpoint admission in this run. Entry zero, inactive entries,
transient observations, interior prefixes, duplicates, probe refusals, and
rejections are ineligible. Choose at most one champion by: greatest
target-provided `SmbProgressWatermark`; fewest actions; ascending raw semantic
input SHA-256 bytes; ascending lane; then ascending entry id.

Verdict **ADOPT** iff that champion exists, is live and probe-surviving by its
normal admission, and its endpoint watermark is strictly greater than
`(7,0,355)`. Embed its exact `SmbInput`, lane, id, lineage, endpoint evidence,
raw-WRAM/input/snapshot hashes, and work. This exact input alone is authorized
as the source of a separately preregistered continuation, which must first
replay and verify it from genesis. Otherwise verdict **STOP** and nothing is
adoptable.

There is one registered live run, no routine replay audit, no automatic rerun,
no post-hoc ranking, and no adoption from any prior unregistered candidate.
Any integrity mismatch authorizes nothing.

## Result

The one registered run completed successfully on `msr1` with verdict
**ADOPT**. The implementation was sealed at commit `9d76a917`, with binary-
source SHA-256
`8526076c3321c4edc8a2890dcff000ddf65b60a5c16c587fc334f038bd89c834`,
module-source SHA-256
`b0e6365a6a90a99c2fc39a8e18e424b7d4a9149956024379228a5309384200d9`,
and release-executable SHA-256
`7fd73341010cf7644b3505bcfa14926072db0c08bfde2f28cacae0bef35ff38e`.
The frozen recipe SHA-256 was
`dce6eeec8e08648c1275bb5f48ccd6ec6a0d6863826392a76e58ff2597c4abec`.

The canonical 17-line, 145,199,634-byte report is stored at
`/root/harmony-smb-sol-p355-harvest-28e9dd86/results/p355-endpoint-harvest-12x700.jsonl`.
Its body SHA-256 is
`7d8670c0972acd53d681d4f9e6be76320942386037b301eba1b1e30cf8096b5d`
and its whole-file SHA-256 is
`8f7ec6d3c8d74c3e1708dc07a8a81c3b8aaf964be4cc7c4f2073c959e515bc42`.
Realized work was 865,191 frames: 157,182 source replay, 45 source probe,
4,693 setup, 345,460 live action, and 357,811 live probe frames.

The registered total order selected lane 11 entry 570, with lineage
`[0,1,2,4,6,7,9,17,22,23,67,124,176,180,184,185,186,189,192,193,218,219,224,228,235,241,242,244,246,247,253,268,332,484,500,569,570]`.
It is a live, probe-surviving, normally retained endpoint at full watermark
`(world=7, level=1, progress=85)` after 3,383 actions and 159,070 absolute
frames. Its mechanical state is `(7,1,85,y=11,engine=8,dead=false,
flag=false)`, raw-WRAM SHA-256 is
`9b08eafed27af5bc8f2355f0492a364908ddf8e53f2e9a26a2162824b04e9777`,
and snapshot SHA-256 is
`9cc13488d2d3b00f3b87b3ef1533c326470a7b5c3ba1adb2785f61512ed5ba4c`.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-p355-harvest-28e9dd86/results/adopted-world-8-2-progress-85-input.json`;
its compact-file and semantic SHA-256 is
`3f10e294a943fbb2fe2dc51cb8877059e01a1cc319167f1df49f12b8d8c02e97`.
This is the registered transition from World 8-1 progress 355 to World 8-2
progress 85. Its from-genesis verification is required before any proposal in
the separately preregistered continuation.
