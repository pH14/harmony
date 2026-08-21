<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p165 confirmatory paired L1-vs-L4 phrase canary

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Question and boundary

The first paired canary produced five L4 wins and three ties, including strict
depth-2-through-4 witnesses, but its preregistered sign gate returned
`NO_GO_L4` because only five pairs were non-tied. This one fresh confirmation
asks whether that generic four-action selection horizon replicates under more
pairs and a new opaque recipe schedule from the independently adopted p165
source. There will be no third L1-vs-L4 confirmation.

The control is `L1`: select a parent for every action. The treatment is `L4`:
select one parent at each four-slot boundary, execute up to four consecutive
opaque actions, and admit every executed prefix using normal `current_parent`
behavior. There is no duration selection, interior-observer admission,
waypoint, route, coordinate, or button-semantic prior.

The binary may read only the exact p165 `SmbInput` and ROM named below. It must
not read the first paired report, v9 report, any archive, stream, manifest,
result, candidate, snapshot, or recipe. No result other than the exact
registered p165 adoption enters initialization, proposal, ranking, or verdict.

## Frozen source and seed

- Code base before this experiment and registered first-pair result commit:
  `e8c3eb00dba5d5cf00bb1c2294a3c76d8eb0a494`.
- Authorizing paired preregistration: `e94d5027`; sealed implementation:
  `782081d4`; registered result commit: `e8c3eb00`; registered report SHA-256:
  `fa57d9118790b97b81147835aa3caa6a5b88eb8126752aa63658cbdedc010242`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p144-l1-l4-e94d5027/results/adopted-world-8-2-progress-165-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `42d92ae8b8a4ed47465302c75c5800b79a54a4990d07b8e1306af75217ce7321`;
  exactly 3,429 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 165 }`; exactly 160,502
  frames; mechanical state `(world=7, level=1, progress=165,
  player_y_bucket=11, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,165,11,8,state_fingerprint=3)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `83b7a658bd1c34828204840087b9c125456177155503eb7bfacbf7d3103f4185`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `fc69d5f71e7ac1d74c17b42eaa1fbf9bc0230bb109d23019cba8d99e7e853cba`.
- Final opaque `ButtonChord { buttons: 16, hold_frames: 113 }`. Registered
  source probe: mask `0x00` survives 45 frames; total source-probe work is 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p165-confirmatory-l1-l4-phrase-canary-v2`;
  label SHA-256
  `291d75929cd4d2cd80214aa56eb69062b47fed43e0693c4eab87a218524c7774`;
  little-endian first-eight-byte master seed `14831150291821600041`.

The standalone binary is `smb-w8-2-p165-confirmatory-l1-l4-canary`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipes or arms, replay the source once from gameplay genesis and verify
every source fact above. From a fresh restore, reproduce the registered
45-frame mask-`0x00` survival, then restore and re-hash the source snapshot.
Use the sealed trace framing from the first paired canary and record the new
trace hash. Any mismatch is integrity **STOP**.

## Fresh frozen paired slots

There are sixteen independent pairs `r=0..15`. Each arm has 128 frozen action
slots `s=0..127`; L4 groups them into 32 phrases. After the baseline passes,
derive with these new domains:

```text
pair_digest = SHA-256(
  master_seed_u64_le || ASCII("confirm-l1-l4-v2-pair") || r_u64_le)
pair_seed = first_8_bytes_as_little_endian_u64(pair_digest)

action_digest = SHA-256(
  pair_seed_u64_le || ASCII("confirm-l1-l4-v2-action") || s_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3429

selector_digest = SHA-256(
  pair_seed_u64_le || ASCII("confirm-l1-l4-v2-parent") || s_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete opaque source `ButtonChord` at `source_index`. There is no
retry, filter, deduplication, semantic inspection, state association, or
outcome feedback. Serialize the pair-major, slot-minor vector
`(r_u64,s_u64,source_index_u64,ButtonChord,selector_seed_u64)` using
`serde_json::to_vec` and record its SHA-256.

For pair `r`, also serialize its exact ordered 128-element projection
`(s_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `r` and
any pair wrapper. Record each projection SHA-256 and require all sixteen exact
projection byte vectors to be pairwise distinct or integrity STOP. Both arms
receive identical slots and seeds. L1 consumes every seed; L4 consumes seeds
only at slots divisible by four and records the rest frozen-but-unused.

## Arms and normal admission

Each of 32 arms starts a fresh archive containing only validated p165 as
`id=0`, `parent_id=None`, `created_execution=0`, directly inserted with no
origin probe. Require one active entry. Every arm uses action limit 4,096;
archive limit 129; `Frozen` key; `ProbeAtAdmission45` masks
`[0x00,0x01,0x81]`; `FewestActions`; existing `ConcentratedRecency`
selection/productivity accounting; and no waypoint, snapback, pinned window,
or empirical chord update. No lineage may exceed `3429+128=3557` actions.

For L1 slot `s`, initialize fresh `StdRand(selector_seed[s])`, call real
`Archive::select_parent` once, restore and verify it, execute the one opaque
action, and process its normal endpoint through snapshot, probe, restore,
duplicate, and admission. Sequence and `created_execution` are `s+1`; record
phrase-prefix depth 1. Then call `record_selection` and
`record_selection_outcome` once for the selected parent. Productive means a
newly allocated retained endpoint; cost is action plus probe work. Any non-Ok
`ExitKind` is integrity STOP. Death with `ExitKind::Ok` ends only this job;
the next slot reselects normally.

For L4 phrase `q=0..31`, let `s=4q`; initialize fresh
`StdRand(selector_seed[s])`, select one parent, restore and verify it, and set
`current_parent` plus cumulative input to that entry. Execute slots `s..s+3`
sequentially from the evolving target. Append each action to cumulative input
and process that normal prefix through the same admission path. At offset
`j=0..3`, record phrase-prefix depth `j+1`; all four candidates use sequence
and `created_execution=q+1`.

On retained admission, update `current_parent` to the new id; on duplicate,
update it to the existing id. Probe refusal or rejection leaves
`current_parent` unchanged while cumulative input and live target continue.
The next admitted prefix uses that `current_parent` as parent id. Any non-Ok
`ExitKind` is integrity STOP. Death with `ExitKind::Ok` ends the phrase, marks
later slots in its group unexecuted-after-death, and resumes with a fresh
selection at the next group. After each phrase, call `record_selection` and
`record_selection_outcome` once for its original selected parent. Productive
means any prefix newly allocated an entry; cost is the phrase's summed action
and probe work.

Thus only selection horizon and ordinary downstream archive feedback differ.
L1 has exactly 128 selections per arm; L4 has exactly 32. No within-action
observation is reconstructed or admitted.

## Pairing, work, and evidence

Arm ordinal is `2r` for L1 and `2r+1` for L4. Use exactly twelve persistent
workers; assign ordinal modulo twelve, execute assigned ordinals ascending,
and return ordinal plus inner success/error. The coordinator buffers replies,
consumes them ascending, and is the sole writer. Any worker, ordinal, restore,
emulator, arithmetic, or report mismatch is integrity STOP.

The deterministic hard bound is:

- 4,096 scheduled full actions: 491,520 frames;
- live per-prefix probes: 552,960 frames;
- one source replay: 160,502 frames;
- one source evidence probe: 45 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 1,209,720 frames. Require exactly 2,048 L1 selections and
512 L4 selections. Record scheduled, executed, and death-skipped slots, work
components, and maximum lineage with checked reconciliation. Expected `msr1`
time is 8–14 minutes; allow 20 operationally. Wall time is not recorded or a
stop.

The create-new NDJSON order is header, source baseline, frozen recipes, arms
ascending, paired classification, adoption classification, summary. Per action
record pair/arm/slot/phrase/depth, recipe, selection, original/current parent,
cumulative input, endpoint state/hashes, probe, admission, active set,
watermark, accounting, and work. The header binds preregistration, source, ROM,
executable, runner sources, recipe, projection, and config hashes.
`body_sha256` covers bytes through the last pre-summary LF; after summary and
LF, flush, sync, and print whole-file SHA-256. No host path, timestamp, or
wall-clock field is permitted.

## Exhaustive structural and adoption decisions

For each arm, take the greatest full target-provided watermark among final
active entries, including source id zero. Pair `r` is an L4 win, L1 win, or tie
by exact watermark order. Let `n` be non-ties and `w` L4 wins. In checked
`u128`, compute exact one-sided tail numerator
`N=sum(k=w..n,choose(n,k))` over denominator `2^n`.

An L4 witness is a final-active, newly allocated, live, probe-surviving L4
endpoint at recorded depth `2..=4`, strictly greater than source `(7,1,165)`,
and strictly greater than its paired L1 final maximum. Record every witness's
pair, id, depth, input, lineage, watermark, and state/input/snapshot hashes.

Structural classification is exhaustive and ordered:

1. **INCONCLUSIVE_SPARSE** iff `n<8`.
2. Otherwise **CONFIRM_L4** iff `80*N <= 2^n` and at least one witness exists.
3. Otherwise **REJECT_L4**.

CONFIRM_L4 promotes only the generic L4 selection horizon for a separately
preregistered search. INCONCLUSIVE_SPARSE does not promote it; REJECT_L4
rejects it. In all cases this exhausts the paired confirmation budget: there
is explicitly no third L1-vs-L4 confirmation and no outcome-dependent recipe.

Independently, adoption-eligible entries are final-active, newly allocated,
normal per-prefix endpoints from either arm. Exclude source, inactive entries,
deaths, refusals/rejections, and duplicates. Rank one global champion by full
watermark; fewest actions; ascending raw semantic input SHA-256; ascending
pair; L1 before L4; then entry id.

Adoption verdict is **ADOPT** iff that champion is live, probe-surviving, and
strictly greater than source `(7,1,165)`. Embed its exact input and complete
arm/depth/lineage/state/hash/work evidence. It is the sole authorized next
source regardless of structural classification. Otherwise verdict is
**STOP** and nothing is adoptable. Any integrity mismatch authorizes nothing.
There is one registered run, no routine replay audit, rerun, or post-hoc
candidate choice.
