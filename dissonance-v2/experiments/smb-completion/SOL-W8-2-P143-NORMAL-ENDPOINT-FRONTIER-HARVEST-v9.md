<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p143 normal-endpoint frontier harvest v9

Status: draft preregistration after the registered v8 result commit and before
implementation seal, recipe materialization, ROM loading, or live candidate
emulation.

## Question and boundary

Run one final unchanged normal-endpoint confirmation from the sole registered
World 8-2 progress-143 adoption. The prior 8,304-draw harvest advanced only
three same-level progress units. This remains one adoptable harvest, not a
policy comparison; its terminal repeat/change rule is frozen below.

The binary may read only the exact adopted `SmbInput` named below and the ROM.
It must not read the v8 report, any archive, stream, manifest, result, candidate,
snapshot, or recipe. No unregistered v8 candidate may enter initialization,
proposal, ranking, or adoption.

Proposal logic treats buttons as opaque and never decodes route, coordinates,
room identity, world, level, progress, or operator goals. The validated source
action marginal, target-defined archive key, existing selector, and
target-provided `SmbProgressWatermark::Ord` are generic retained-artifact/domain
adapters, not operator priors.

## Frozen source and seed

- Code base before this experiment and registered v8 result commit:
  `6365540e5398f08f06b8de3f4fe9e5cecef77713`.
- Authorizing v8 preregistration: `879baaa6`; sealed implementation:
  `20f04b7d`; registered report whole-file SHA-256:
  `da45db6702f5c4e1623623812307f699ee60992e585b74e738c1972e5442a0ff`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p140-harvest-879baaa6/results/adopted-world-8-2-progress-143-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `96e4eeef6a968fc4c3705875c581f78ddc054250366b53b0a0d1783b9e5b36cd`;
  exactly 3,409 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 143 }`; exactly 159,748
  frames; mechanical state `(world=7, level=1, progress=143,
  player_y_bucket=7, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,143,7,8,state_fingerprint=8)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `c84a4bcf5c5da85a8e6141f798806721bc3d40ad9c113e778846c0bddc177a1c`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `302b85270e398f4b21808a71ab5276bb691b92ad5e4f03fcfe9a2ae36e85c943`.
- Registered final action: opaque `ButtonChord { buttons: 2,
  hold_frames: 7 }`. Registered source probe: mask `0x00`, 45 frames, alive
  and survived.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p143-normal-endpoint-frontier-harvest-v9`;
  label SHA-256
  `a3fc3dc6fe5a4c0c8c94f2563a91f7dc5d73600cc5090dbe49afed43027ba98a`;
  little-endian first-eight-byte master seed `886183276979289251`.

The standalone binary is `smb-w8-2-p143-endpoint-frontier-harvest`; positional
arguments are `<input.json> <create-new-output.jsonl>`, and the ROM is read only
from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and 16 MiB using
maximum plus one.

Before materializing a proposal or initializing a lane, replay the source once
from gameplay genesis. Verify every source identity and replay-derived fact
above, including the exact endpoint raw WRAM, snapshot, maximum, key,
milestones, final action, and mechanics. Run the registered 45-frame mask
`0x00` source probe, require survival, and restore and re-hash the snapshot.
Use the sealed trace framing from v8 and record the newly computed trace hash.
Any mismatch is integrity **STOP** before recipes or live draws.

## Frozen recipes

After the baseline passes, materialize twelve independent lanes `l = 0..11`,
each with 680 serial draws `d = 0..679`:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3409

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

Each lane starts a new archive containing only the validated World 8-2 p143
source as `id=0`, `parent_id=None`, `created_execution=0`. Insert it directly
from the validated input/key/milestones/snapshot with no origin probe or added
emulation, and require exactly one active entry.

Every lane uses action limit 4,096; archive limit 681; `Frozen` key;
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

The source has 3,409 actions and each lane has only 680 draws, so no lineage
may exceed 4,089 actions. Record each lane's maximum lineage length and treat
any value above 4,089 as an integrity STOP; the 4,096 action limit cannot bind
an otherwise registered proposal.

## Work, evidence, and verdict

The deterministic hard bound is:

- 8,160 full actions: 979,200 frames;
- live endpoint probes: 1,101,600 frames;
- one source replay: 159,748 frames;
- one source evidence probe: 45 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 2,245,286 frames. Record and reconcile every component with
checked arithmetic. Crossing any component or total bound is integrity STOP.
Expected `msr1` live time is 16–24 minutes; allow 32 minutes operationally.
Wall time is neither recorded nor a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, lanes
ascending, adoption classification, terminal repeat/change classification,
then summary. Record enough per-draw archive, selector, input, snapshot, probe,
lineage, active-set, and work evidence to recompute every final value. The
header binds this preregistration, source, ROM, executable, runner sources,
recipe, and config hashes. `body_sha256` covers exact bytes through the final
pre-summary LF; after summary and LF, flush, sync, and print the whole-file
SHA-256. No host path, timestamp, or wall-clock field is permitted.

Eligible entries are only final-active entries newly allocated through normal
ordinary endpoint admission in this run. Entry zero, inactive entries,
transient observations, interior prefixes, duplicates, probe refusals, and
rejections are ineligible. Choose at most one champion by: greatest full
target-provided `SmbProgressWatermark`; fewest actions; ascending raw semantic
input SHA-256 bytes; ascending lane; then ascending entry id.

Verdict **ADOPT** iff that champion exists, is live and probe-surviving by its
normal admission, and its full endpoint watermark is strictly greater than
`(7,1,143)`. Embed its exact `SmbInput`, lane, id, lineage, endpoint evidence,
raw-WRAM/input/snapshot hashes, and work. This exact input alone is authorized
as a source for a separately preregistered next search. Otherwise verdict
**STOP** and nothing is adoptable.

This is the final unchanged endpoint-only confirmation. Separately classify
`REPEAT_ENDPOINT` only if the champion's target-provided `(world,level)` prefix
is lexicographically greater than `(7,1)`. If verdict is STOP, or if an ADOPT
champion advances only progress within `(7,1)`, classify `CHANGE_STRUCTURE`:
do not preregister another repeat of this endpoint-only recipe. A same-level
ADOPT remains the sole valid source for that structurally different search.
This repeat/change rule is exhaustive and independent of route, coordinate,
and action semantics.

There is one registered live run, no routine replay audit, no automatic rerun,
no post-hoc ranking, and no adoption from any prior unregistered candidate.
Any integrity mismatch authorizes nothing.

## Result

The one registered run completed successfully on `msr1` with adoption verdict
**ADOPT** and terminal classification **CHANGE_STRUCTURE**. The implementation
was sealed at commit `de9d2389`, with binary-source SHA-256
`29299021df8383e3bd75168071be3c41c6ef1c662d70401705fda73b28516975`,
module-source SHA-256
`a004339cff6dec4d647172d3d779dcbc5883811957d149bd61d2f9737b567b12`,
and release-executable SHA-256
`113c0a368a9fe5403c1844a98ddbe6baf55a838abdddc2ab08a0e31a29f31589`.
The frozen recipe SHA-256 was
`65f73c6acf90ee61277640f07244b90e78b119ad50c65dd6934818acf001e4b1`.

The canonical 18-line, 134,101,384-byte report is stored at
`/root/harmony-smb-sol-w8-2-p143-harvest-1b0cb8cb/results/w8-2-p143-endpoint-harvest-12x680.jsonl`.
Its body SHA-256 is
`56bcee418c398c2c8e85c644adb6aea4f8541a6f5b3c3b006350dd379ab7c14f`
and whole-file SHA-256 is
`406674cd375a313db0c4857300659d899ff6d80ffbbc39caf58c487882328aca`.
Realized work was 911,202 frames: 159,748 source replay, 45 source probe,
4,693 setup, 183,420 live action, and 563,296 live probe frames.

The registered total order selected lane 0 entry 5, with lineage `[0,4,5]`.
It is a live, probe-surviving, normally retained endpoint at full watermark
`(world=7, level=1, progress=144)` after 3,411 actions and 159,757 absolute
frames. Its mechanical state is `(7,1,144,y=9,engine=8,dead=false,
flag=false)`, raw-WRAM SHA-256 is
`824f46256fc9e29ad0e6fa8cfaf3b801f5f555af81ff35863e617f6330bcf436`,
and snapshot SHA-256 is
`531fc0ca4b8178dbe7d3fbf99b994f5678138d73ffcbe014429384348dac36ce`.
Its registered admission probe killed masks `0x00` and `0x01` after 39 frames
each; mask `0x81` survived all 45 frames.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-w8-2-p143-harvest-1b0cb8cb/results/adopted-world-8-2-progress-144-input.json`;
its compact-file and semantic SHA-256 is
`8c162945c26f6544c390e659002422c8065f4b18535cdb8e75e7922b1d558025`.
The registered terminal rule prohibits another unchanged endpoint-only
harvest. The separately preregistered paired L1-vs-L4 canary changes only
parent-selection coherence while retaining normal per-prefix admission.
