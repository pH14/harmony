<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p196 paired B1-vs-B2 sibling-burst canary

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Question and boundary

Normal one-action FULL-marginal search advanced the registered source from
progress 183 to 196, while trailing-256 sampling was not promoted. Its 32 arms
retained 64..102 active entries and usually selected each distinct parent only
once. This fresh paired canary asks whether spending two independent opaque
actions on one selected parent improves candidate emission without chaining
actions or deleting archive diversity.

`B1` is the control and selects a parent before every action. `B2` is the
treatment and selects one parent for each two-action burst, then independently
restores that same original parent before each sibling chord. Both candidates
are ordinary one-action endpoints. Candidate two is always a sibling of
candidate one, never its descendant.

Both arms use the same full-source opaque chord marginal, selector seeds,
archive, probes, admission, and ranking. There is no button-semantic,
duration-selection, route, coordinate, waypoint, state/action-association, or
outcome prior. The binary may read only the exact p196 `SmbInput` and ROM named
below. It must not read the p183 FULL-vs-TAIL256 report or any other report,
archive, result, candidate, snapshot, stream, manifest, or recipe.

## Frozen source and seed

- Code base before this experiment and registered p196 result commit:
  `c045412f1575f9921a86347ff8ea75a69d0565f2`.
- Authorizing p183 preregistration: `d8ef4322`; sealed implementation:
  `5a4635f9`; registered result commit: `c045412f`; registered report SHA-256:
  `7014812f683986c83f246eebd78e8efe9b98ff1576e5760e2fd1e9f269d88203`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p183-full-tail256-d8ef4322/results/adopted-world-8-2-progress-196-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `72f6dc1ed54ef824c73c794e03410b9d64502ede032fc8b787d4ac67763b403d`;
  exactly 110,605 bytes and 3,445 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 196 }`; exactly 161,116
  frames; mechanical state `(world=7, level=1, progress=196,
  player_y_bucket=6, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,196,6,8,state_fingerprint=9)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `49b2721d7533f4c45249d60ce9ec715e2ef2d5d2c1e19776bd6e2ef75d4c2e80`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `0627939cc2ca87cbdeea4e74705a09145150f22b7b6d88543a63e4365b201c83`.
- Final opaque `ButtonChord { buttons: 131, hold_frames: 74 }`. Registered
  source probe: mask `0x00` survives all 45 frames; total source-probe work 45.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-2-p196-paired-b1-b2-sibling-burst-v1`; label
  SHA-256
  `cc83cf81c4262aa15a68a65b32772b0cb8af2dc0dffd00f8f71929142ef9e958`;
  little-endian first-eight-byte master seed `11613137214561551308`.

The standalone binary is `smb-w8-2-p196-paired-b1-b2-sibling-canary`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipe generation or arm execution, replay the source once from
gameplay genesis and verify every source fact above. From a fresh restore,
reproduce the registered mask-`0x00` 45-frame survival, then restore and
re-hash the source snapshot. Record the sealed trace framing and trace hash.
Any mismatch is integrity **STOP**.

## Fresh frozen paired recipe

There are sixteen independent pairs `r=0..15`. Each arm has 128 action slots
`s=0..127`; B2 groups them into 64 bursts `q=0..63`, with slots `2q` and
`2q+1`. After the source baseline passes, derive:

```text
pair_digest = SHA-256(
  master_seed_u64_le || ASCII("p196-b1-b2-pair") || r_u64_le)
pair_seed = first_8_bytes_as_little_endian_u64(pair_digest)

action_digest = SHA-256(
  pair_seed_u64_le || ASCII("p196-b1-b2-action") || s_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3445

selector_digest = SHA-256(
  pair_seed_u64_le || ASCII("p196-b1-b2-parent") || s_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

Copy the complete source `ButtonChord` at `source_index`. There is no retry,
filtering, deduplication, semantic inspection, state association, or outcome
feedback. Both arms receive the identical 128 chords and selector seeds. B1
consumes every seed; B2 consumes only even-slot seeds and records odd-slot
seeds as frozen-but-unused.

Serialize the pair-major, slot-minor 2,048-element vector
`(r_u64,s_u64,source_index_u64,ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its byte length and SHA-256. For each pair,
serialize and hash one bare `Vec` in slot order whose exact projection element
is `(s_u64,source_index_u64,ButtonChord,selector_seed_u64)`, excluding `r` and
any wrapper. Before arms, require all sixteen exact projection byte vectors to
be pairwise distinct; collision is integrity STOP with no retry. Freeze every
slot before execution. Do not deduplicate recipes or statistical outcomes;
normal per-arm archive duplicate detection remains unchanged.

## Identical archive and B1 control

Each of 32 arms starts a fresh archive containing only validated p196 as
`id=0`, `parent_id=None`, `created_execution=0`, directly inserted without an
origin probe. Require one active entry. Every arm uses action limit 4,096;
archive limit 129; `Frozen` key; `ProbeAtAdmission45` masks
`[0x00,0x01,0x81]`; `FewestActions`; existing `ConcentratedRecency`
selection/productivity accounting; and no waypoint, snapback, pinned window,
phrase, compaction, or empirical chord update. No lineage may exceed
`3445+128=3573` actions.

For B1 slot `s`, initialize fresh `StdRand(selector_seed[s])`, call real
`Archive::select_parent` once, restore and verify it, build
`parent_input + chord[s]`, execute that one action, and process the normal
endpoint through snapshot, ordered probe, restore, duplicate, and admission.
Sequence and `created_execution` are `s+1`. Call `record_selection` and
`record_selection_outcome` once on the selected parent. Productive means the
candidate newly allocates; cost is its action plus probe work.

## B2 sibling treatment

For burst `q`, let `s=2q`. Initialize fresh `StdRand(selector_seed[s])`, call
real `Archive::select_parent` exactly once, and freeze the selected original
parent id, snapshot hash, and input. For sibling offsets `j=0,1`, independently
restore and verify that same original snapshot, build exactly
`original_parent_input + chord[s+j]`, execute one action, and process its
normal endpoint in offset order through snapshot, ordered probe, restore,
duplicate, and admission. Both siblings have sequence and
`created_execution=q+1`, and both name the original selected parent as parent.

Candidate-one retention, duplicate resolution, refusal, rejection, or death
must not change candidate two's parent, input prefix, or start snapshot. Death
of candidate one does not skip candidate two. Any non-Ok `ExitKind`, worker
error, or emulator error in either arm is integrity STOP; death with
`ExitKind::Ok` ends only that candidate. After both siblings, call
`record_selection` and `record_selection_outcome` exactly once on the original
parent. Productive means either sibling newly allocates; cost is the sum of
both candidates' action and probe work.

Thus the only treatment difference is two independent candidate emissions per
selection. B1 has 128 selections per arm and B2 has 64; both schedule exactly
128 ordinary one-action endpoints.

## Pairing, work, and report

Arm ordinal is `2r` for B1 and `2r+1` for B2. Use exactly twelve persistent
workers; assign ordinal modulo twelve, execute assigned ordinals ascending,
and return ordinal plus inner success/error. The coordinator buffers replies,
consumes them ascending, and is the sole writer. Any ordinal, restore,
arithmetic, accounting, or report mismatch is integrity STOP.

The deterministic hard bound is 491,520 scheduled action frames; 552,960 live
probe frames; 161,116 source replay frames; 45 source-probe frames; and 4,693
setup frames from thirteen targets at 361 each. Total hard bound is
**1,210,334 frames**. Require exactly 4,096 scheduled and executed candidates,
2,048 B1 selections, and 1,024 B2 selections. Record all work components,
active counts, sibling starts/parents, and maximum lineage with checked
reconciliation. Expected `msr1` time is 8–14 minutes; allow 20 operationally.
Wall time is not recorded or a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, arms
ascending, paired classification, adoption classification, summary. Per
candidate record pair, arm, slot, burst, sibling offset, recipe, selector use,
original parent/input/snapshot, candidate input, endpoint state/hashes, probe,
admission, active set, accounting, and work. The header binds preregistration,
source, ROM, executable, runner sources, recipe/projections, trace, and config
hashes. `body_sha256` covers bytes through the last pre-summary LF; after
summary and LF, flush, sync, and print whole-file SHA-256. No host path,
timestamp, or wall-clock field is permitted.

## Exhaustive structural and adoption decisions

For each arm, take the greatest full target-provided watermark among final
active entries, including source id zero. Pair `r` is a B2 win, B1 win, or tie
by exact watermark order. Let `n` be non-ties and `w` B2 wins. Without outcome
deduplication, compute in checked `u128` the exact one-sided tail numerator
`N=sum(k=w..n,choose(n,k))` over denominator `2^n`.

A B2 witness is a final-active, newly allocated, live, probe-surviving **second
sibling** (`j=1`) strictly greater than source `(7,1,196)` and strictly greater
than its paired B1 final maximum. Record every witness's pair, id, slot, burst,
original parent, both sibling recipes, input, lineage, watermark, and
state/input/snapshot hashes.

Structural classification is exhaustive and ordered:

1. **INCONCLUSIVE_SPARSE** iff `n<8`.
2. Otherwise **PROMOTE_B2** iff `80*N <= 2^n` and at least one witness exists.
3. Otherwise **RETAIN_B1**.

PROMOTE_B2 promotes only generic independent two-sibling emission for a
separately preregistered search. The other verdicts do not promote it. This
exhausts the comparison: there is no repeat, relaxed gate, or outcome-dependent
recipe.

Independently, adoption-eligible entries are final-active, newly allocated,
normal one-action endpoints from either arm and either B2 sibling. Exclude
source, inactive entries, deaths, refusals/rejections, and duplicates. Rank one
global champion by full watermark; fewest actions; ascending raw semantic
input SHA-256; ascending pair; B1 before B2; then entry id.

Adoption verdict is **ADOPT** iff that champion is live, probe-surviving, and
strictly greater than source `(7,1,196)`. Embed its exact input and complete
arm/slot/burst/sibling/lineage/state/hash/work evidence. It is the sole
authorized next source regardless of structural classification. Otherwise
verdict is **STOP** and nothing is adoptable. Any integrity mismatch authorizes
nothing. There is one registered run, no routine replay audit, rerun, or
post-hoc candidate choice.

## Result

The one registered run completed successfully on `msr1` with structural
verdict **INCONCLUSIVE_SPARSE** and adoption verdict **ADOPT**. The
implementation was sealed at commit `762a03a0`, with binary-source SHA-256
`141e10e3d039943106e567a57ac9b26cb5b890867305ef366026213da05b185c`,
module-source SHA-256
`ac0264b75d7f967d4ccf1667029769012ec4f9b8476359635189529f9e787fde`,
and release-executable SHA-256
`1bb8496110da06dec250f2a6818c9501b6477bdf37eb7021434d979220ce7eaf`.
The frozen 132,496-byte recipe SHA-256 was
`806e5c4d3200d983c7043aded109bd516463e543bba66c2867b24dd3e0484131`.

The canonical 38-line, 555,637,719-byte report is stored at
`/root/harmony-smb-sol-w8-2-p196-b1-b2-ee578070/results/w8-2-p196-paired-b1-b2-16x128.jsonl`.
Its body SHA-256 is
`8e9c4bdd12d3bf9e06812c3610ff30eee6daf190873e5e02cca7c661ac58b64b`
and whole-file SHA-256 is
`a8f8edee757d5328c21cd3485f0671ad398540dcbd2668a9002661812d6aff21`.
Realized work was 524,151 frames: 161,116 source replay, 45 source evidence
probe, 4,693 setup, 122,773 live action, and 235,524 live probe frames.

There were seven non-ties: B2 won five, B1 won two, and nine pairs tied. Thus
`n=7`, `w=5`, and the exact one-sided sign tail was `29/128`. There were 107
strict second-sibling B2 witnesses, but the exhaustive first gate classified
the result INCONCLUSIVE_SPARSE because `n<8`. B2 is not promoted, B1 remains
the registered policy, and the preregistered no-repeat rule closes this
comparison.

The independent adoption order selected pair 9, arm B2, entry 46, created at
slot 105 in burst 52 as sibling offset 1, with lineage `[0,6,14,24,27,46]`.
It is a live, probe-surviving, normally retained endpoint at full watermark
`(world=7, level=1, progress=213)` after 3,450 actions and 161,449 absolute
frames. Its mechanical state is
`(7,1,213,y=11,engine=5,dead=false,flag=true)`, raw-WRAM SHA-256 is
`6f1b96d92cfc62464fde03fc725a55334b1000a260f462e1cd63d067486e6e62`,
and snapshot SHA-256 is
`38b772afb3fd1cb73f344fca6bf79dd48eda663aebfc2e16418812793f17d367`.
Its frozen key fingerprint is 47, final chord is
`ButtonChord { buttons: 2, hold_frames: 103 }`, and its admission probe
survived mask `0x00` for all 45 frames.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-w8-2-p196-b1-b2-ee578070/results/adopted-world-8-2-progress-213-input.json`;
its 110,764-byte compact-file and semantic SHA-256 is
`18fe08991e9f53de44ca0231e71306101d3db6846d1f58d05bde74851aa76c7a`.
This source is adoptable independently of the sparse structural result. No B2
promotion or next experiment is authorized by this result note alone.
