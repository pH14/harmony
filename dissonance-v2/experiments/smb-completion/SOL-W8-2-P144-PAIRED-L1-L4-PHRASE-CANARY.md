<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p144 paired L1-vs-L4 phrase canary

Status: draft preregistration after the registered v9 result commit and before
implementation seal, recipe materialization, ROM loading, or live candidate
emulation.

## Question and boundary

The final unchanged endpoint harvest advanced only one same-level progress unit
and returned **CHANGE_STRUCTURE**. This adoptable paired canary tests the
smallest generic structural change: does holding one selected parent across a
four-action phrase improve the final retained watermark relative to ordinary
one-action re-selection, while every executed action boundary still follows
normal admission?

The control is `L1`: select a parent for every action. The treatment is `L4`:
select one parent at each four-slot boundary, execute up to four consecutive
opaque actions, and admit each executed prefix in order using the campaign's
normal `current_parent` behavior. There is no duration selection, interior
observer admission, waypoint, route, coordinate, or button-semantic prior.

The binary may read only the exact p144 `SmbInput` and ROM named below. It must
not read the v9 report, any archive, stream, manifest, result, candidate,
snapshot, or recipe. No other v9 state is an input.

## Frozen source and seed

- Code base before this experiment and registered v9 result commit:
  `e56f1301567433b054ba009894adb68f9164d43a`.
- Authorizing v9 preregistration: `1b0cb8cb`; sealed implementation:
  `de9d2389`; registered v9 report SHA-256:
  `406674cd375a313db0c4857300659d899ff6d80ffbbc39caf58c487882328aca`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p143-harvest-1b0cb8cb/results/adopted-world-8-2-progress-144-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `8c162945c26f6544c390e659002422c8065f4b18535cdb8e75e7922b1d558025`;
  exactly 3,411 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 144 }`; exactly 159,757
  frames; mechanical state `(world=7, level=1, progress=144,
  player_y_bucket=9, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,144,9,8,state_fingerprint=2)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `824f46256fc9e29ad0e6fa8cfaf3b801f5f555af81ff35863e617f6330bcf436`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `531fc0ca4b8178dbe7d3fbf99b994f5678138d73ffcbe014429384348dac36ce`.
- Final opaque `ButtonChord { buttons: 131, hold_frames: 3 }`. Registered
  source probe, in order: masks `0x00` and `0x01` die at 39 frames; mask
  `0x81` survives 45 frames; total work 123 frames.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p144-paired-l1-l4-phrase-canary-v1`;
  label SHA-256
  `3a7b6d0eb01aab0e3043c7fe3c2e90d72d74765af41538b2f8e7081171a00dd3`;
  little-endian first-eight-byte master seed `1056967881007135546`.

The standalone binary is `smb-w8-2-p144-paired-l1-l4-phrase-canary`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipes or arms, replay the source once from gameplay genesis and verify
every source fact above. From a fresh restore for each mask, reproduce the
registered `39/39/45` source-probe transcript in order, then restore and re-hash
the source snapshot. Use the sealed trace framing from v9 and record the new
trace hash. Any mismatch is integrity **STOP**.

## Frozen paired slots

There are eight independent pairs `r=0..7`. Each arm has 256 frozen action
slots `s=0..255`; L4 groups them into 64 phrases. After the baseline passes,
derive:

```text
pair_digest = SHA-256(
  master_seed_u64_le || ASCII("paired-l1-l4-pair") || r_u64_le)
pair_seed = first_8_bytes_as_little_endian_u64(pair_digest)

action_digest = SHA-256(
  pair_seed_u64_le || ASCII("paired-l1-l4-action") || s_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3411

selector_digest = SHA-256(
  pair_seed_u64_le || ASCII("paired-l1-l4-parent") || s_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete opaque source `ButtonChord` at `source_index`. There is no
retry, filter, deduplication, semantic inspection, state association, or
outcome feedback. Serialize the pair-major, slot-minor vector
`(r_u64,s_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its SHA-256. Separately define pair `r`'s
distinctness identity as the exact `serde_json::to_vec` bytes of its ordered
256-element projection
`(s_u64,source_index_u64,ButtonChord,selector_seed_u64)`, explicitly excluding
`r` and any pair wrapper; record each projection SHA-256. Require those eight
exact projection byte vectors to be pairwise distinct or integrity STOP. Both
arms receive the identical 256 slots and selector seeds. L1 consumes every
seed; L4 consumes only seeds for slots divisible by four and records the other
seeds as frozen-but-unused.

## Arms and normal admission

Each of sixteen arms starts a fresh archive containing only validated p144 as
`id=0`, `parent_id=None`, `created_execution=0`, inserted directly with no
origin probe. Require exactly one active entry. Every arm uses action limit
4,096; archive limit 257; `Frozen` key; `ProbeAtAdmission45` masks
`[0x00,0x01,0x81]`; `FewestActions`; existing `ConcentratedRecency`
selection/productivity accounting; and no waypoint, snapback, pinned window,
or empirical chord update. No lineage may exceed `3411+256=3667` actions.

For L1 slot `s`, initialize fresh `StdRand(selector_seed[s])`, call real
`Archive::select_parent` once, restore and verify that parent, execute the one
opaque action, and process its endpoint through the ordinary snapshot, probe,
restore, duplicate, and admission path. Its sequence and
`created_execution` are `s+1`; record phrase-prefix depth `1`. Then call
`record_selection` and
`record_selection_outcome` once for the selected parent; productive means the
slot newly allocated a retained endpoint, and cost is its action plus probe
work. Any `ExitKind` other than `Ok` is integrity STOP. Death with
`ExitKind::Ok` is a valid outcome and ends only this one-action job; slot `s+1`
still reselects normally.

For L4 phrase `q=0..63`, let `s=4q`; initialize fresh
`StdRand(selector_seed[s])`, call `select_parent` once, restore and verify it,
and set both `current_parent` and cumulative input to that selected entry.
Execute actions at slots `s..s+3` sequentially from the evolving target. After
each action, append it to the cumulative input and process that normal boundary
through the same snapshot, probe, restore, duplicate, and archive admission.
For phrase offset `j=0..3`, record phrase-prefix depth `j+1` on the executed
boundary, its candidate, and any retained or duplicate entry. All four
candidates use sequence and `created_execution=q+1`.

On newly retained admission, set `current_parent` to the new id; on duplicate,
set it to the existing id. On probe refusal or archive rejection, leave
`current_parent` unchanged while the cumulative input and live target continue.
The next admitted prefix uses the resulting `current_parent` as `parent_id`.
Any `ExitKind` other than `Ok` is integrity STOP. If an action dies with
`ExitKind::Ok`, end the phrase, mark later slots in that four-slot group
unexecuted-after-death, and reselect normally at the next four-slot boundary.
After each phrase, call `record_selection` and `record_selection_outcome` once
for its originally selected parent; productive means any executed prefix newly
allocated an entry, and cost is the sum of all executed action and probe work
in that phrase.

Thus the arms differ only in selection horizon and its ordinary downstream
archive feedback: L1 has exactly 256 parent selections per pair arm and L4 has
exactly 64. No observation inside an action is reconstructed or admitted.

## Pairing, work, and evidence

Arm ordinal is `2r` for L1 and `2r+1` for L4. Use exactly twelve persistent
workers; assign ordinal modulo twelve, execute assigned ordinals ascending,
and return ordinal plus inner success/error. The coordinator buffers and
consumes all replies ascending and is the sole writer. Missing, duplicate,
wrong-worker, restore, emulator, arithmetic, or report errors are integrity
STOP.

The deterministic hard bound is:

- 4,096 scheduled full actions: 491,520 frames;
- live per-prefix probes: 552,960 frames;
- one source replay: 159,757 frames;
- registered source evidence probes: 123 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 1,209,053 frames. Also require exactly 2,048 L1 parent
selections and 512 L4 parent selections. Record scheduled, executed, and
terminal-skipped slots, all work components, and each arm's maximum lineage;
reconcile with checked arithmetic. Expected `msr1` time is 8–14 minutes; allow
20 minutes operationally. Wall time is neither recorded nor a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, arm
records ascending, paired classification, adoption classification, summary.
Per action record pair/arm/slot/phrase, recipe, selection evidence, original
and current parent, cumulative input, phrase-prefix depth, endpoint state and
hashes, probe transcript, admission decision, active set, watermark,
accounting, and work. The header binds preregistration, source, ROM,
executable, runner sources, recipe, and config hashes. `body_sha256` covers
bytes through the final pre-summary LF; after summary and LF, flush, sync, and
print whole-file SHA-256. No host path, timestamp, or wall-clock field is
permitted.

## Frozen paired and adoption decisions

For each arm, take the greatest full target-provided watermark among its final
active archive entries, including source id zero. Pair `r` is an L4 win, L1
win, or tie by exact watermark order. Let `n` be non-ties and `w` L4 wins.
Compute the exact one-sided sign tail
`N/2^n = sum(k=w..n, choose(n,k))/2^n` in checked `u128` arithmetic.

An L4 structural witness is a final-active, newly allocated, live,
probe-surviving L4 endpoint whose recorded phrase-prefix depth is in `2..=4`,
whose full watermark is strictly greater than source `(7,1,144)`, and whose
full watermark is strictly greater than the final maximum of its paired L1
arm. Record every witness's pair, entry, depth, input, lineage, watermark, and
state/input/snapshot hashes.

Structural verdict is **GO_L4** iff `n>=7`, `80*N <= 2^n`, and at least one L4
structural witness exists; otherwise it is **NO_GO_L4**. With eight pairs the
sign condition requires L4 to win every non-tie and at least seven pairs. All
eight pairs are reported; no outcome deduplication or later recipe is allowed.
GO_L4 promotes only the generic four-action selection horizon for a separately
preregistered search.

Independently, eligible adoption entries are final-active, newly allocated
normal per-prefix endpoints from either arm. Exclude source, inactive entries,
deaths, rejected/probe-refused candidates, and duplicates. Rank one global
champion by greatest full watermark; fewest actions; ascending raw semantic
input SHA-256; ascending pair; L1 before L4; then entry id.

Adoption verdict is **ADOPT** iff that champion is live, probe-surviving, and
strictly greater than source watermark `(7,1,144)`. Embed its exact `SmbInput`
and full lineage/state/hash/work evidence, including arm and recorded phrase-
prefix depth. It is the sole authorized next source regardless of
GO_L4/NO_GO_L4. Otherwise adoption verdict is **STOP** and nothing is
adoptable. Any integrity mismatch authorizes nothing. There is one registered
run, no routine replay audit, rerun, or post-hoc candidate choice.

## Result

The one registered run completed successfully on `msr1` with structural
verdict **NO_GO_L4** and adoption verdict **ADOPT**. The implementation was
sealed at commit `782081d4`, with binary-source SHA-256
`01a71e23f275573387b533b47da9f4e93b9e39308f346468a98559bd305f178c`,
module-source SHA-256
`8449bd1dc5de94d3e50f096fafe6b0bcbe9e4dce84634e1576a950ee776e82cd`,
and release-executable SHA-256
`ba890e7658892d3ea711dea48d3aceba60eae21ef4676fb54a8633fc37be14a6`.
The frozen recipe SHA-256 was
`1ffca21f9ab3136fedb0e74abff3aaabc153a8393e799d4598453d53b9e3b2d1`.

The canonical 22-line, 98,379,501-byte report is stored at
`/root/harmony-smb-sol-w8-2-p144-l1-l4-e94d5027/results/w8-2-p144-paired-l1-l4-8x256.jsonl`.
Its body SHA-256 is
`ad9b65183318f5d536cfeb7047f7a22481a494bb0f017c781a21767bb29daa33`
and whole-file SHA-256 is
`fa57d9118790b97b81147835aa3caa6a5b88eb8126752aa63658cbdedc010242`.
Realized work was 435,212 frames: 159,757 source replay, 123 source evidence
probes, 4,693 setup, 72,476 live action, and 198,163 live probe frames.

Pairs 0, 1, and 3 tied at source progress 144. L4 won pairs 2, 4, 5, 6,
and 7 with maxima 165, 146, 146, 148, and 155 respectively; every L1 maximum
was 144. Thus `n=5`, `w=5`, and the exact one-sided sign tail was `1/32`.
Depth-2-through-4 strict L4 witnesses existed, but the preregistered `n>=7`
sign gate did not pass, so L4 is not promoted.

The independent adoption order selected pair 2, arm L4, entry 121, at recorded
phrase-prefix depth 1, with lineage
`[0,1,2,3,4,6,7,10,11,34,35,100,101,102,104,105,121]`. It is a live,
probe-surviving, normally retained endpoint at full watermark
`(world=7, level=1, progress=165)` after 3,429 actions and 160,502 absolute
frames. Its mechanical state is `(7,1,165,y=11,engine=8,dead=false,
flag=false)`, raw-WRAM SHA-256 is
`83b7a658bd1c34828204840087b9c125456177155503eb7bfacbf7d3103f4185`,
and snapshot SHA-256 is
`fc69d5f71e7ac1d74c17b42eaa1fbf9bc0230bb109d23019cba8d99e7e853cba`.
Its admission probe survived mask `0x00` for all 45 frames.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-w8-2-p144-l1-l4-e94d5027/results/adopted-world-8-2-progress-165-input.json`;
its compact-file and semantic SHA-256 is
`42d92ae8b8a4ed47465302c75c5800b79a54a4990d07b8e1306af75217ce7321`.
This source is adoptable independently of the failed structural gate. No L4
policy promotion or next experiment is authorized by this result note alone.
