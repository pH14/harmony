<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-2 p183 paired FULL-vs-TAIL256 action-marginal canary

Status: preregistered before recipe materialization, ROM loading, or live
candidate emulation.

## Question and boundary

The registered fixed-L4 confirmation rejected a four-action selection horizon
while independently adopting a normal one-action L1 endpoint at World 8-2
progress 183. This fresh paired canary asks one smaller structural question:
does drawing whole opaque chords from the trailing 256 actions of the current
retained input outperform drawing them from its full action marginal?

`FULL` is the control and indexes all 3,440 source actions. `TAIL256` is the
treatment and indexes only the generic trailing 256-action window. Both arms
otherwise use the same real one-action selector, probes, archive, admission,
and ranking. The tail window is a position-only retained-artifact prior that a
production searcher can derive generically from its current input. Neither the
operator nor the binary may inspect button meaning, route, coordinates,
waypoints, state/action association, outcomes, or selected durations.

The binary may read only the exact p183 `SmbInput` and ROM named below. It must
not read either L1-vs-L4 report, any earlier campaign report, archive, stream,
manifest, result, candidate, snapshot, or recipe. No post-hoc result enters
initialization, proposal, ranking, or verdict.

## Frozen source and seed

- Code base before this experiment and registered p183 result commit:
  `734191b103a4106282349bd286afa1eabbf1d48a`.
- Authorizing confirmation preregistration: `d64cc8de`; sealed confirmation
  implementation: `98aa20a5`; registered result commit: `734191b1`;
  registered report SHA-256:
  `9fa87e073313acfa571c56f9b6004dc7e18de1fef5edab7c24030470a4a15230`.
- Exact source file:
  `/root/harmony-smb-sol-w8-2-p165-l1-l4-confirm-d64cc8de/results/adopted-world-8-2-progress-183-input.json`.
- Compact-file and semantic `SmbInput` SHA-256:
  `c56360d445ece8c6df51153943c7ab593a5639a92f9057f31907618b35cc0112`;
  exactly 110,445 bytes and 3,440 actions.
- Registered replay endpoint: alive `ExitKind::Ok`; maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 1, progress: 183 }`; exactly 160,902
  frames; mechanical state `(world=7, level=1, progress=183,
  player_y_bucket=9, player_engine_state=8, dead=false, flag_active=false)`;
  frozen key `(7,1,183,9,8,state_fingerprint=55)`.
- Registered milestones: `max_1_1_scroll_bucket=195`,
  `reached_1_1_flag=true`, `reached_1_2=true`, `reached_onward=true`.
- Raw-WRAM SHA-256:
  `37a3fe9b0285edf6ec9ac6ff23c3d6c1d4da64f12a5f280f6aed89737f47d160`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `dfb7d4a391a00f8340294887ca30b7abff3e200d3b7130fd8cf0042641af1098`.
- Final opaque `ButtonChord { buttons: 129, hold_frames: 9 }`. The ordered
  registered source probe dies under mask `0x00` after 26 frames, then survives
  mask `0x01` for 45 frames; total source-probe work is 71.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label
  `sol-restart-w8-2-p183-paired-full-tail256-action-marginal-v1`; label
  SHA-256
  `864ef8e409a480588b1cd8629996ced6f651fc8443177dd7569049285e79ce02`;
  little-endian first-eight-byte master seed `6377277434759761542`.

The standalone binary is `smb-w8-2-p183-paired-full-tail256-canary`; its
positional arguments are `<input.json> <create-new-output.jsonl>`, and it reads
the ROM only from `HARMONY_SMB_ROM`. Cap source and ROM reads at 2 MiB and
16 MiB using maximum plus one.

Before recipe generation or arm execution, replay the source once from
gameplay genesis and verify every source fact above. From fresh restores,
reproduce the ordered mask-`0x00` death at 26 frames and mask-`0x01` survival
through 45 frames, then restore and re-hash the source snapshot. Record the
sealed trace framing and trace hash. Any mismatch is integrity **STOP**.

## Fresh frozen paired recipes

There are sixteen independent pairs `r=0..15`. Each arm has 128 one-action
slots `s=0..127`. After the source baseline passes, derive:

```text
pair_digest = SHA-256(
  master_seed_u64_le || ASCII("p183-full-tail256-pair") || r_u64_le)
pair_seed = first_8_bytes_as_little_endian_u64(pair_digest)

rank_digest = SHA-256(
  pair_seed_u64_le || ASCII("p183-full-tail256-rank") || s_u64_le)
rank_word = first_8_bytes_as_little_endian_u64(rank_digest)
full_index = rank_word mod 3440
tail_index = 3440 - 256 + (rank_word mod 256)

selector_digest = SHA-256(
  pair_seed_u64_le || ASCII("p183-full-tail256-parent") || s_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(selector_digest)
```

For `FULL`, copy the complete source `ButtonChord` at `full_index`; for
`TAIL256`, copy the complete chord at `tail_index`. A pair shares each
`rank_word` and `selector_seed`. There is no retry, filtering, deduplication,
semantic inspection, state association, or outcome feedback.

Serialize the pair-major, slot-minor 2,048-element recipe vector
`(r_u64,s_u64,rank_word_u64,full_index_u64,FULL_ButtonChord,
tail_index_u64,TAIL256_ButtonChord,selector_seed_u64)` with
`serde_json::to_vec` and record its byte length and SHA-256. For each pair,
also serialize and hash one bare `Vec` in slot order whose exact 128 projection
elements are `(s_u64,rank_word_u64,full_index_u64,FULL_ButtonChord,
tail_index_u64,TAIL256_ButtonChord,selector_seed_u64)`, excluding `r` and any
pair wrapper. Before any arm, require all sixteen exact projection byte
vectors to be pairwise distinct; any collision is integrity STOP with no
retry. Separately, require the exact 128-chord `FULL` action vector to differ
from the exact 128-chord `TAIL256` action vector within every pair or integrity
STOP. Freeze all recipes before any arm; do not deduplicate identical chords
or recipes, and do not deduplicate arm or pair classifications merely because
outcomes match. Normal per-arm archive duplicate detection remains unchanged.

## Identical one-action search arms

Each of 32 arms starts a fresh archive containing only validated p183 as
`id=0`, `parent_id=None`, `created_execution=0`, directly inserted without an
origin probe. Require one active entry. Every arm uses action limit 4,096;
archive limit 129; `Frozen` key; `ProbeAtAdmission45` masks
`[0x00,0x01,0x81]`; `FewestActions`; existing `ConcentratedRecency`
selection/productivity accounting; and no waypoint, snapback, pinned window,
phrase, observer event, or empirical chord update. No lineage may exceed
`3440+128=3568` actions.

For slot `s`, initialize fresh `StdRand(selector_seed[s])`, call the real
`Archive::select_parent` exactly once, restore and verify the selected entry,
execute the arm's complete opaque chord, and process the normal endpoint
through snapshot, ordered probe, restore, duplicate, and admission. Sequence
and `created_execution` are `s+1`. Call `record_selection` and
`record_selection_outcome` once for the selected parent. Productive means a
newly allocated retained endpoint; cost is action plus probe work.

Any non-Ok `ExitKind`, worker error, or emulator error is integrity STOP.
Death with `ExitKind::Ok` ends only that slot; the next slot selects normally.
The two arms differ only in the indexed chord. Parent ids, endpoint outcomes,
and archive evolution may consequently differ and are evidence, not recipe
inputs.

## Pairing, work, and report

Arm ordinal is `2r` for `FULL` and `2r+1` for `TAIL256`. Use exactly twelve
persistent workers; assign ordinal modulo twelve, execute each worker's
ordinals ascending, and return ordinal plus inner success/error. The
coordinator buffers replies, consumes them ascending, and is the sole writer.
Any ordinal, restore, arithmetic, accounting, or report mismatch is integrity
STOP.

The deterministic hard bound is:

- 4,096 scheduled full actions: 491,520 frames;
- live per-prefix probes: 552,960 frames;
- one source replay: 160,902 frames;
- ordered source evidence probes: 71 frames; and
- thirteen target setups at 361 frames each: 4,693 frames.

Total hard bound: 1,210,146 frames. Require exactly 4,096 selections and 4,096
scheduled slots. Record executed actions, work components, active counts, and
maximum lineage with checked reconciliation. Expected `msr1` time is 8–14
minutes; allow 20 operationally. Wall time is not recorded or a stop.

The create-new NDJSON order is header, source baseline, frozen recipes, arms
ascending, paired classification, adoption classification, summary. Per slot
record pair, arm, slot, rank word, both indices and chords, selector seed,
selection, cumulative input, endpoint state/hashes, ordered probe, admission,
active set, watermark, accounting, and work. The header binds preregistration,
source, ROM, executable, runner sources, recipe/projection, trace, and config
hashes. `body_sha256` covers bytes through the last pre-summary LF; after
summary and LF, flush, sync, and print whole-file SHA-256. No host path,
timestamp, or wall-clock field is permitted.

## Exhaustive structural and adoption decisions

For each arm, take the greatest full target-provided watermark among final
active entries, including source id zero. Pair `r` is a `TAIL256` win, `FULL`
win, or tie by exact watermark order. Let `n` be non-ties and `w` TAIL256
wins. Without outcome deduplication, compute in checked `u128` the exact
one-sided tail numerator `N=sum(k=w..n,choose(n,k))` over denominator `2^n`.

A TAIL256 witness is a final-active, newly allocated, live, probe-surviving
TAIL256 endpoint strictly greater than source `(7,1,183)` and strictly greater
than its paired FULL final maximum. Its creation slot's TAIL256 chord must
differ from the FULL chord at that same slot. Record every witness's pair, id,
slot, rank word, indices, both chords, input, lineage, watermark, and
state/input/snapshot hashes.

Structural classification is exhaustive and ordered:

1. **INCONCLUSIVE_SPARSE** iff `n<8`.
2. Otherwise **PROMOTE_TAIL256** iff `80*N <= 2^n` and at least one witness
   exists.
3. Otherwise **RETAIN_FULL**.

PROMOTE_TAIL256 promotes only generic trailing-256 whole-chord sampling for a
separately preregistered search; it never hard-codes this source or any chord.
The other verdicts do not promote it. This exhausts the comparison: there is
no repeat, relaxed gate, or outcome-dependent recipe.

Independently, adoption-eligible entries are final-active, newly allocated
normal one-action endpoints from either arm. Exclude source, inactive entries,
deaths, refusals/rejections, and duplicates. Rank one global champion by full
watermark; fewest actions; ascending raw semantic input SHA-256; ascending
pair; FULL before TAIL256; then entry id.

Adoption verdict is **ADOPT** iff that champion is live, probe-surviving, and
strictly greater than source `(7,1,183)`. Embed its exact input and complete
arm/slot/lineage/state/hash/work evidence. It is the sole authorized next
source regardless of structural classification. Otherwise verdict is
**STOP** and nothing is adoptable. Any integrity mismatch authorizes nothing.
There is one registered run, no routine replay audit, rerun, or post-hoc
candidate choice.

## Result

The one registered run completed successfully on `msr1` with structural
verdict **RETAIN_FULL** and adoption verdict **ADOPT**. The implementation was
sealed at commit `5a4635f9`, with binary-source SHA-256
`8e17361aebd0929413ef61c3ee8e596eaaa68ca50bc42041d2a012e3f5905ff5`,
module-source SHA-256
`9d5f88db3bfadf966f4e6149a0e4ddb0b0a3a584b20088b3e6a1ab3d16780d76`,
and release-executable SHA-256
`b21c690f7b7320fdde5600bf3f9d79a9603ddd4044f1d2ba2b57f5bb5b559456`.
The frozen 250,741-byte recipe SHA-256 was
`039cfd75d3aee68251b3a20dae93b467dac3b5d794ec12b9ca69b8081f4933e0`.

The canonical 38-line, 564,361,594-byte report is stored at
`/root/harmony-smb-sol-w8-2-p183-full-tail256-d8ef4322/results/w8-2-p183-paired-full-tail256-16x128.jsonl`.
Its body SHA-256 is
`e5b1868bf824904c7cc421b88514eea359c84c1ac960c12f2b77eb6103193e2e`
and whole-file SHA-256 is
`7014812f683986c83f246eebd78e8efe9b98ff1576e5760e2fd1e9f269d88203`.
Realized work was 462,655 frames: 160,902 source replay, 71 source evidence
probes, 4,693 setup, 142,456 live action, and 154,533 live probe frames.

There were thirteen non-ties: TAIL256 won seven, FULL won six, and three pairs
tied. Thus `n=13`, `w=7`, and the exact one-sided sign tail was
`4,096/8,192`. Ninety-three strict TAIL256 witnesses existed, but the sealed
sign gate did not pass. Trailing-256 action-marginal sampling is not promoted;
the generic full-source marginal remains the registered policy, with no repeat
of this comparison.

The independent adoption order selected pair 5, arm FULL, entry 58, created at
slot 105, with lineage `[0,1,2,3,34,58]`. It is a live, probe-surviving,
normally retained endpoint at full watermark `(world=7, level=1,
progress=196)` after 3,445 actions and 161,116 absolute frames. Its mechanical
state is `(7,1,196,y=6,engine=8,dead=false,flag=false)`, raw-WRAM SHA-256 is
`49b2721d7533f4c45249d60ce9ec715e2ef2d5d2c1e19776bd6e2ef75d4c2e80`,
and snapshot SHA-256 is
`0627939cc2ca87cbdeea4e74705a09145150f22b7b6d88543a63e4365b201c83`.
Its frozen key fingerprint is 9, final chord is
`ButtonChord { buttons: 131, hold_frames: 74 }`, and its admission probe
survived mask `0x00` for all 45 frames.

The sole authorized input was extracted byte-for-byte to
`/root/harmony-smb-sol-w8-2-p183-full-tail256-d8ef4322/results/adopted-world-8-2-progress-196-input.json`;
its 110,605-byte compact-file and semantic SHA-256 is
`72f6dc1ed54ef824c73c794e03410b9d64502ede032fc8b787d4ac67763b403d`.
This source is adoptable independently of the rejected structural treatment.
No TAIL256 promotion or next experiment is authorized by this result note
alone.
