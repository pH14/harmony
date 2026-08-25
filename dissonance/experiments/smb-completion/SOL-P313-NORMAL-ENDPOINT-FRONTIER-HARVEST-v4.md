<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol p313 normal-endpoint frontier harvest v4

Status: preregistered before implementation seal, recipe materialization, ROM
loading, or live candidate emulation.

## Question and boundary

Continue the existing normal SMB endpoint search from the sole registered p313
adoption. This is one adoptable harvest, not a policy comparison. The immutable
p313 endpoint is the comparator and every live draw is ordinary endpoint search.

The binary may read only the exact adopted `SmbInput` named below and the ROM.
It must not read the v3 report, any archive, stream, manifest, result, candidate,
snapshot, or recipe. No unregistered v3 candidate may enter initialization,
proposal, ranking, or adoption.

Proposal logic treats buttons as opaque and never decodes route, coordinates,
room identity, world, level, progress, or operator goals. The validated source
action marginal, target-defined archive key, existing selector, and
target-provided `SmbProgressWatermark::Ord` are generic retained-artifact/domain
adapters, not operator priors.

## Frozen source and seed

- Code base before this experiment: `d4afe722`.
- Authorizing v3 preregistration: `04ddd630`; sealed implementation:
  `236e3db5`; registered result: `d4afe722`.
- Exact source file:
  `/root/harmony-smb-sol-p305-harvest-04ddd630/results/adopted-progress-313-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `8609428ac01d7925a24c2c452d61930839a1ddb3defebfcbe624a5d9d0d831a1`;
  exactly 3,326 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 0, progress: 313 }`; exactly 156,397
  frames; mechanical state `(world=7, level=0, progress=313,
  player_y_bucket=8, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,0,313,8,8,state_fingerprint=17)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `d18108d71075dd53b5b9599fbc15573092dda698757b606463edb2ee00d458b2`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `76d67dfcc9e8b97e00cbb7ba37a0365b32a4f08b436df6dd0d376f68bfa0e903`.
- Registered final action: opaque `ButtonChord { buttons: 129,
  hold_frames: 11 }`. Registered source probe: mask `0x00`, 45 frames,
  alive and survived.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-p313-normal-endpoint-frontier-harvest-v4`;
  label SHA-256
  `285d5cd8df0a8c4408382fd65fe0eed04af5c80da1637d102cf8b09dea8c1767`;
  little-endian first-eight-byte master seed `4939334847842508072`.

The standalone binary is `smb-p313-endpoint-frontier-harvest`; positional
arguments are `<input.json> <create-new-output.jsonl>`, and the ROM is read only
from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and 16 MiB using
maximum plus one.

Before materializing a proposal or initializing a lane, replay the source once
from gameplay genesis. Verify every source identity and replay-derived fact
above, including the exact endpoint raw WRAM, snapshot, maximum, key,
milestones, final action, and mechanics. Run the registered 45-frame mask
`0x00` source probe, require survival, and restore and re-hash the snapshot.
Use the sealed trace framing from v3 and record the newly computed trace hash.
Any mismatch is integrity **STOP** before recipes or live draws.

## Frozen recipes

After the baseline passes, materialize twelve independent lanes `l = 0..11`,
each with 512 serial draws `d = 0..511`:

```text
lane_digest = SHA-256(
  master_seed_u64_le || ASCII("normal-endpoint-lane") || l_u64_le)
lane_seed = first_8_bytes_as_little_endian_u64(lane_digest)

action_digest = SHA-256(
  lane_seed_u64_le || ASCII("normal-endpoint-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3326

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

Each lane starts a new archive containing only the validated p313 source as
`id=0`, `parent_id=None`, `created_execution=0`. Insert it directly from the
validated input/key/milestones/snapshot with no origin probe or added
emulation, and require exactly one active entry.

Every lane uses action limit 4,096; archive limit 513; `Frozen` key;
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

## Work, evidence, and verdict

The deterministic hard bound is:

- 6,144 full actions: 737,280 frames;
- live endpoint probes: 829,440 frames;
- one source replay: 156,397 frames;
- one source evidence probe: 45 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 1,727,855 frames. Record and reconcile every component with
checked arithmetic. Crossing any component or total bound is integrity STOP.
Expected `msr1` live time is 12–18 minutes; allow 24 minutes operationally.
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
`(7,0,313)`. Embed its exact `SmbInput`, lane, id, lineage, endpoint evidence,
raw-WRAM/input/snapshot hashes, and work. This exact input alone is authorized
as the source of a separately preregistered continuation, which must first
replay and verify it from genesis. Otherwise verdict **STOP** and nothing is
adoptable.

There is one registered live run, no routine replay audit, no automatic rerun,
no post-hoc ranking, and no adoption from any prior unregistered candidate.
Any integrity mismatch authorizes nothing.

## Result

The one registered run completed successfully on `msr1` with verdict
**ADOPT**. The sealed implementation commit was `a97a24d2`; release-executable
SHA-256 was
`2c1dda5dd45b1af1fb46226daf4cbfe53e9c8929c95b22175a37a79667a7c3c2`,
and frozen recipe SHA-256 was
`2149e9bb5abeb58c79ef4cbf6897a3e7cf881a30242242d01128069cc4f34d81`.

The canonical 17-line report is
`/root/harmony-smb-sol-p313-harvest-9a2176a5/results/p313-endpoint-harvest-12x512.jsonl`.
It is 100,010,571 bytes with body SHA-256
`1e3a9e746d97d7300681a60238b7ae518bf10f44b69fce7942ae5100d353880b`
and whole-file SHA-256
`b60f74128034a295819bccef6c9bde3f7675f663b5822fb968078205aaf7307c`.
Standard error was empty. Realized work was 641,399 frames: 156,397 source
replay, 45 source-probe, 4,693 setup, and 480,264 experimental frames.

Final world 8-1 lane watermarks were
`[348, 348, 351, 348, 348, 351, 347, 348, 349, 348, 350, 329]`.
The registered order selected lane 2 entry 232, lineage
`[0, 1, 2, 5, 6, 14, 15, 40, 44, 46, 48, 53, 55, 67, 91, 121, 182, 232]`.
It is a live, probe-surviving normal endpoint at progress 351 after 3,343
actions and 157,113 frames. Its semantic input SHA-256 is
`432b52a21e283e2e3eff67e9e9a4cc4d1d6d2c1cebfa8afeb159d1de42fc3f40`,
raw-WRAM SHA-256 is
`2a3fc640d9de11a3ac2b674c46ad45abcb13231b7337193f2f73755236204f2c`,
and snapshot SHA-256 is
`276ea34bdeb81b8ca8e0f501365b34d76a31357c73c4c55f06618da80c685989`.

The sole authorized input was extracted to
`/root/harmony-smb-sol-p313-harvest-9a2176a5/results/adopted-progress-351-input.json`,
whose file SHA-256 equals the semantic input hash. Its required verification
is assigned to the next separately preregistered continuation's baseline.
