<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p140 normal-endpoint frontier harvest v8

Status: draft preregistration after the registered v7 result commit and before
implementation seal, recipe materialization, ROM loading, or live candidate
emulation.

## Question and boundary

Continue the existing normal SMB endpoint search from the sole registered
World 8-2 progress-140 adoption. This is one adoptable harvest, not a policy
comparison. The immutable source endpoint is the comparator and every live
draw is ordinary endpoint search.

The binary may read only the exact adopted `SmbInput` named below and the ROM.
It must not read the v7 report, any archive, stream, manifest, result, candidate,
snapshot, or recipe. No unregistered v7 candidate may enter initialization,
proposal, ranking, or adoption.

Proposal logic treats buttons as opaque and never decodes route, coordinates,
room identity, world, level, progress, or operator goals. The validated source
action marginal, target-defined archive key, existing selector, and
target-provided `SmbProgressWatermark::Ord` are generic retained-artifact/domain
adapters, not operator priors.

## Frozen source and seed

- Code base before this experiment and registered v7 result commit:
  `efa091a1f9dca531d7da2f8d3256d73600393889`.
- Authorizing v7 preregistration: `cdde38f8`; sealed implementation:
  `b397dec5`; registered report whole-file SHA-256:
  `356f255958d05ecc0ef5aa4f4cd488f29db47d687650a5a2113a74e02137373b`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p85-harvest-cdde38f8/results/adopted-world-8-2-progress-140-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `2aec47372f9772f452967bc2088d09b31df43111e081bc0bdc21c42ae22e66ef`;
  exactly 3,404 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 140 }`; exactly 159,699
  frames; mechanical state `(world=7, level=1, progress=140,
  player_y_bucket=11, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,140,11,8,state_fingerprint=35)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `a3d2a3ec85d1f450c37bb6c39e61ac8425c4bdead045f86e3cf9f0f31536dbc4`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `418d226b9f90ad33bf1e388a0184b5d7265e1104ed6e336e1a1592f5bba2eac4`.
- Registered final action: opaque `ButtonChord { buttons: 1,
  hold_frames: 120 }`. Registered source probe, in order: mask `0x00` died at
  37 frames; mask `0x01` died at 37 frames; mask `0x81` remained alive and
  survived 45 frames. Total registered source-probe work is 119 frames.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p140-normal-endpoint-frontier-harvest-v8`;
  label SHA-256
  `aceeaa197c15e68128ce6e7057ebe07b7475f12d259b5a85d4709841a0528afc`;
  little-endian first-eight-byte master seed `9360192498286915244`.

The standalone binary is `smb-w8-2-p140-endpoint-frontier-harvest`; positional
arguments are `<input.json> <create-new-output.jsonl>`, and the ROM is read only
from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and 16 MiB using
maximum plus one.

Before materializing a proposal or initializing a lane, replay the source once
from gameplay genesis. Verify every source identity and replay-derived fact
above, including the exact endpoint raw WRAM, snapshot, maximum, key,
milestones, final action, and mechanics. From a fresh restore of the validated
source snapshot for each mask, reproduce the registered source probe in order:
`0x00` dies at frame 37, `0x01` dies at frame 37, and `0x81` survives 45 frames.
Restore and re-hash the source snapshot afterward. Use the sealed trace framing
from v7 and record the newly computed trace hash. Any mismatch is integrity
**STOP** before recipes or live draws.

## Frozen recipes

After the baseline passes, materialize twelve independent lanes `l = 0..11`,
each with 692 serial draws `d = 0..691`:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3404

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

Each lane starts a new archive containing only the validated World 8-2 p140
source as `id=0`, `parent_id=None`, `created_execution=0`. Insert it directly
from the validated input/key/milestones/snapshot with no origin probe or added
emulation, and require exactly one active entry.

Every lane uses action limit 4,096; archive limit 693; `Frozen` key;
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

The source has 3,404 actions and each lane has only 692 draws, so no lineage
may exceed 4,096 actions. Record each lane's maximum lineage length and treat
any value above 4,096 as an integrity STOP. No registered proposal can exceed
the unchanged action limit.

## Work, evidence, and verdict

The deterministic hard bound is:

- 8,304 full actions: 996,480 frames;
- live endpoint probes: 1,121,040 frames;
- one source replay: 159,699 frames;
- registered source evidence probes: 119 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 2,282,031 frames. Record and reconcile every component with
checked arithmetic. Crossing any component or total bound is integrity STOP.
Expected `msr1` live time is 16–24 minutes; allow 32 minutes operationally.
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
rejections are ineligible. Choose at most one champion by: greatest full
target-provided `SmbProgressWatermark`; fewest actions; ascending raw semantic
input SHA-256 bytes; ascending lane; then ascending entry id.

Verdict **ADOPT** iff that champion exists, is live and probe-surviving by its
normal admission, and its full endpoint watermark is strictly greater than
`(7,1,140)`. Embed its exact `SmbInput`, lane, id, lineage, endpoint evidence,
raw-WRAM/input/snapshot hashes, and work. This exact input alone is authorized
as the source of a separately preregistered continuation, which must first
replay and verify it from genesis. Otherwise verdict **STOP** and nothing is
adoptable.

There is one registered live run, no routine replay audit, no automatic rerun,
no post-hoc ranking, and no adoption from any prior unregistered candidate.
Any integrity mismatch authorizes nothing.
