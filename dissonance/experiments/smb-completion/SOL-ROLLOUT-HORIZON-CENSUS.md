<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol paired rollout-continuity census

Status: preregistered before recipe materialization or emulator execution.

## Question

C119 used one-or-two-action jobs. The blind trace-replacement canary was null,
and the common-parent graph could not supply a balanced long-phrase corpus.
This census asks a smaller causal question: from the exact C119 winner state,
can a fixed block of opaque actions reach a live, mechanically deeper action
boundary because it carries state forward from preceding actions, when the
identical block does not do so after a reset to the C119 winner?

This is a paired mechanism canary, not a campaign-policy comparison. A positive
result says only that uninterrupted state carry matters under the frozen
retained-step distribution from this source. It does not establish admission
probe viability or archive retention, and does not prove that atomic long jobs
beat a campaign which chains retained one-or-two-action parents.

## Frozen identities

- Code base before this experiment: `14605677`.
- C119 archive SHA-256:
  `d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81`.
- C119 stream SHA-256:
  `ab869286a526dab104f7846ae0313745de7087e3733e99016218defb42e90201`.
- Selected entry `48076`, parent `29805`, created at execution `49709`;
  3,297 actions; compact source byte SHA-256
  `5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159`;
  semantic input SHA-256
  `584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1`.
- Recovered C119 production binary SHA-256:
  `87fb11f300a7af9386eb06c8b55e7a7353d6cb3654b83ee6a5615806e72e2862`.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Verified source outcome: alive with `ExitKind::Ok`, maximum and endpoint
  `SmbProgressWatermark { world: 7, level: 0, progress: 236 }`, at exactly
  155,148 absolute frames. The endpoint `SmbMechanicalState` is
  `(world=7, level=0, progress=236, player_y_bucket=7,
  player_engine_state=8, dead=false, flag_active=false)`.
- Sealed source replay evidence from the first Sol canary: trace SHA-256
  `9245f6d42f684a1fcd0a33a762519a51270d1ece2b695ea5a575d83ff64149a1`,
  final raw-WRAM SHA-256
  `936ac08d4c48a2968bec111324fd7ed28628ea89b35baa049b1b5abfffc896ea`,
  and final `SmbSnapshot` canonical-JSON SHA-256
  `107bab5a4691ca0e43586b3c95849031782d40f2a3013856161ae4f1d997ae66`.
- Seed label `sol-restart-c119-continuous-rollout-v1`, SHA-256
  `d0a86c80cac50cec33f1a6a55db713f93468796f14f554cdcc040f5d000a9d60`;
  master seed `0xec0cc5ca806ca8d0` (`17009187366200191184`).

The executable and configuration hashes are recorded before outcomes. Source,
ROM, and executable reads are bounded at 2 MiB, 16 MiB, and 256 MiB
respectively, using maximum plus one byte and rejecting overflow. All counts
and ranges use checked arithmetic.

## Frozen recipes

There are exactly 100 independent streams `s = 0..99`, each with 32 actions
`k = 0..31`. Before constructing an emulator, derive

`digest = SHA-256(master_seed_u64_le || ASCII("rollout-corpus-index") ||
s_u64_le || k_u64_le)`

and interpret the first eight digest bytes as little-endian `u64`. Reduce it
modulo 3,297 and copy the complete opaque `ButtonChord` at that index from the
frozen selected input. There is no retry, deduplication, position window,
outcome feedback, or semantic inspection of buttons or duration. Sampling
with replacement deterministically samples the selected input's empirical
marginal step distribution up to the stated modulo reduction; it never copies
action order or state association. This is an explicit artifact-derived
empirical prior from retained search output, not operator-authored Mario
knowledge. Any later searcher experiment must derive it generically from its
own retained inputs rather than hard-code this source or its action values, so the canary's
claim remains explicitly source-conditioned.

Serialize the complete `Vec<Vec<ButtonChord>>` with `serde_json::to_vec` and
record its SHA-256 before loading the ROM. Equal recipes are allowed.

For each `(s,H)`, define `pair_recipe_sha256` as SHA-256 of
`serde_json::to_vec(stream[0..H])`. Within each `H`, insert pairs into a
`BTreeMap<pair_recipe_sha256, stream>` in ascending stream order and retain the
first stream for each exact recipe. This symmetric, outcome-independent
pair-level deduplication is the only deduplication used by the decision gate.

The registered horizons are `H = [2,4,8,16,32]`. For each horizon let
`M = H / 2`. One paired comparison uses:

- the **continuous band**: stream actions with zero-based indices `[M,H)`
  executed after the same stream's prefix `[0,M)` without resetting; and
- the **reset band**: the identical actions `[M,H)` restored and executed from
  the exact C119 winner snapshot.

The continuous and reset bands therefore have identical opaque actions,
order, count, multiset, and requested hold-frame sum. The only changed variable
is whether state produced by `[0,M)` crosses the midpoint. The five reset bands
`[1,2)`, `[2,4)`, `[4,8)`, `[8,16)`, and `[16,32)` are disjoint within a stream.
Every continuous stream and every registered reset band is executed regardless
of earlier outcomes; outcomes never select later work.

## Exact execution and identity

Replay the 3,297-action source once from gameplay genesis. Validate its byte
and semantic hashes, action bounds, exact baseline frames, alive endpoint,
maximum watermark, raw WRAM hash, full snapshot hash, and deterministic trace
hash against the sealed values above. The source trace uses the frozen first
Sol canary framing: initialize SHA-256 with ASCII
`"smb-trace-canary-v1\0trace\0"`; frame the gameplay-genesis initial
observation as `length_u64_le || serde_json::to_vec(initial_observation)`; then
for each zero-based action index append its `u64` little-endian bytes and frame
`serde_json::to_vec(action)` and `serde_json::to_vec(action_observations)` the
same way. Freeze the exact endpoint snapshot.

For each stream in ascending order:

1. restore the endpoint snapshot and execute `[0,32)` sequentially, recording
   every completed action boundary until death; then
2. for each `H` in ascending registered order, restore the endpoint snapshot
   and execute only `[M,H)`, recording every completed reset-band boundary
   until death.

Any `ExitKind` other than `Ok` is an integrity STOP, not an experimental
outcome. Death is an outcome and stops only the current branch. A later death
never erases an earlier live boundary.

For continuous one-based boundary `j` (`1..=32`), define:

- `candidate_sha256 = SHA-256(serde_json::to_vec(SmbInput {
  actions: source.actions || stream[0..j] }))`; and
- `suffix_sha256 = SHA-256(serde_json::to_vec(stream[0..j]))`.

For reset band `[M,H)` and boundary `j` (`M < j <= H`), define:

- `candidate_sha256 = SHA-256(serde_json::to_vec(SmbInput {
  actions: source.actions || stream[M..j] }))`; and
- `suffix_sha256 = SHA-256(serde_json::to_vec(stream[M..j]))`.

The first expression denotes vector concatenation, not serialized punctuation.
These hashes identify the exact logical inputs reproduced by their snapshots.
Candidate hashes identify evidence records, while the frozen pair-recipe hash
above is the symmetric deduplication identity for paired inference.

Each candidate trace clones the live source trace hasher at the C119 endpoint
and uses the same frozen action framing. A continuous action at stream index
`k` is framed with logical input index `3,297 + k`; a reset-band action at
stream index `k` is framed with logical input index `3,297 + (k - M)`. Checked
arithmetic is required. Thus each trace hash is exactly the trace of the
logical `SmbInput` named by its `candidate_sha256`.

After every completed action boundary record:

- branch (`continuous` or `midpoint_reset`), stream, `H`/`M` when applicable,
  and one-based source-stream action index;
- copied source-corpus index and complete opaque action;
- exact candidate and suffix hashes above;
- requested cumulative branch hold frames and the matched logical-prefix hold
  frames for a reset branch;
- absolute gameplay-genesis frames and frames since the latest restore;
- executed logical action count;
- death and endpoint `SmbMechanicalState`;
- endpoint and maximum-observed `SmbProgressWatermark` through this boundary;
- raw WRAM, full `SmbSnapshot` canonical-JSON, and deterministic trace hashes;
  and
- whether any transient observer event, and separately the live endpoint,
  strictly exceeded the baseline watermark.

The restored observation frame must equal 155,148. Every absolute frame count
must reconcile as 155,148 plus frames since restore. `frames_clocked()` is a
separately delimited emulator-work counter because it is instance-cumulative
and is not restored; branch work must equal the observation-frame delta. A
death-shortened action records actual frames separately from requested holds.

The create-new canonical NDJSON report order is header, baseline, recipes,
then for each stream its continuous boundaries followed by reset bands in
ascending `H`, then the summary. It records all artifact/config/recipe hashes,
baseline evidence, every completed boundary, the frozen summaries below, and
`body_sha256`: SHA-256 over the exact UTF-8 NDJSON bytes from the header through
the final pre-summary record, including every terminating LF. After writing the
summary and its terminating LF, flushing, and syncing, the executable prints
SHA-256 over the complete file separately; that self-referential value is not
a report field. The output path is not a report field. There is no wall-clock
field, campaign/archive mutation, routine replay audit, or automatic larger run.

## Paired classification

All progress comparisons use the derived lexicographic `Ord` of
`SmbProgressWatermark { world, level, progress }`. Transient watermarks are
diagnostic only; the decision uses live action-boundary endpoints because those
are the states the campaign can consider for admission.

For each `(s,H)`:

- `eligible`: the continuous branch completed action `M` alive and no live
  continuous boundary in `[1,M]` exceeded the baseline watermark;
- `continuity_live_deeper`: eligible, and at least one live continuous boundary in
  `(M,H]` exceeded baseline;
- `reset_live_deeper`: eligible, and at least one live reset boundary while
  executing `[M,H)` from the winner snapshot exceeded baseline;
- `continuity_win`: `continuity_live_deeper && !reset_live_deeper`;
- `reset_win`: `reset_live_deeper && !continuity_live_deeper`.

If both branches are live-deeper, or neither is, the pair is not a win. A
live-deeper branch has one evidence record: its first qualifying live boundary,
identified by that boundary's `candidate_sha256`. Raw pair classification and
raw counts never depend on duplicates. The decision considers only the
canonical first stream for each `pair_recipe_sha256` at that `H`, so a complete
pair is either included once or excluded symmetrically before outcomes are
counted. Candidate-hash uniqueness is reported only as a diagnostic and cannot
change the gate. A later death within the band does not erase an earlier
live-deeper record. Because eligibility requires no earlier continuous
advance, a stream can receive at most one continuity win across the dyadic
bands.

For each `H`, the summary records exact integer counts of eligible pairs,
continuity-live-deeper pairs, reset-live-deeper pairs, both-live-deeper pairs,
raw continuity wins, raw reset wins, continuous branches alive through `H`,
and reset bands alive through their ends. It also records the canonical pair
recipe count, the deduplicated discordant-pair counts described below, and the
ordered raw evidence
`(stream, action, candidate_sha256, watermark, absolute_frames,
frames_since_restore)` records and exact total emulator-work frames for the
continuous executions (counted once) and reset bands. Counts are recomputed
from the records rather than maintained by an independent mutable accumulator.

## Frozen decision

`H=2` is calibration only because C119 already searched one-or-two-action
suffixes. Define the eligible experimental horizons as `H = [4,8,16,32]`.

For each eligible `H`, after the symmetric pair-recipe deduplication and
eligibility filter, let `C_H` be the number of discordant continuity wins and
`R_H` the number of discordant reset wins. Let `N_H = C_H + R_H`. Compute the
one-sided exact sign-test tail with integer arithmetic only:

`tail_numerator_H = sum(combination(N_H,i), i=C_H..N_H)`

over denominator `2^N_H`. All values fit `u128` because `N_H <= 100`; construct
binomial coefficients with checked integer arithmetic and treat overflow as an
integrity STOP. The Bonferroni-adjusted threshold for four registered eligible
horizons is exactly `1/80`, tested without floating point as
`tail_numerator_H * 80 <= 2^N_H`.

GO only if at least one single eligible horizon has `C_H > R_H` and passes that
exact threshold. This requires directional paired evidence at one frozen
horizon (for example `7-0`, while `3-2` cannot pass). Otherwise STOP at this
scale: do not enlarge the census or change a searcher policy from this result.

On GO, define `H*` as the smallest eligible horizon passing the exact test. The
exact same binary, seed, source/ROM/executable paths, and configuration must
first produce a byte-identical canonical report at a second create-new output
path; the output path is deliberately excluded from canonical bytes. A
mismatch is an integrity stop. A GO then authorizes
only a separately preregistered paired campaign canary at `H*`; it never
directly promotes a policy.

## Sealed result

Status: **STOP**. The one registered live run completed on `msr1`; the GO gate
failed, so there was no exact rerun and no searcher-policy change is authorized
from this experiment.

- Implementation commit: `37afff66cc51ce29ec0af4a7ad19f91d5a22cfb7`.
- Temporary implementation source SHA-256:
  `af321eeba46d6d44eb1f5087336de3f465028edde364cc856a7c86c797664f4a`.
- Release executable SHA-256:
  `5962e08a5ac935ab8fd589149d6de8dfb8b5a0ec569d378c6b3bef1b7b7c3f4e`.
- Config SHA-256:
  `ef340d10afd17ca98c3a19e6b4a9730019f713ee4a5e2c625849e23daaaa7786`.
- Recipe SHA-256:
  `a000be41bdc99f2b7c0fd1b35d8a35770c0f377b446d17ea724735d8454800a2`.
- Canonical report on `msr1`:
  `/root/harmony-smb-sol-rollout-234cf6b1/results/c119-rollout-horizon-100-prereg-234cf6b1.jsonl`;
  1,848 lines, 2,469,475 bytes; body SHA-256
  `6e38e487442db7d069cbfebda51c8c4f3cb33b07d63556750ed001caa44adea6`;
  whole-file SHA-256
  `a2a5e698e05e1dc4acfe7f8b3243d8930d159a499ffe95ec226ddfd1bba2af69`.
- Appended emulator work: 16,491 continuous frames and 53,900 reset-band
  frames, in addition to the one validated 155,148-frame source replay.

The paired results were:

| H | eligible | continuity wins | reset wins | exact one-sided tail | adjusted gate |
|---:|---:|---:|---:|---:|:---|
| 2 | 75 | 0 | 3 | 8/8 | calibration only |
| 4 | 49 | 6 | 5 | 1024/2048 | fail |
| 8 | 18 | 4 | 0 | 1/16 | fail |
| 16 | 5 | 0 | 0 | 1/1 | fail |
| 32 | 0 | 0 | 0 | 1/1 | fail |

The directional `4-0` at `H=8` is hypothesis-generating but does not meet the
frozen family-wise threshold of `1/80`; it must not be promoted, enlarged, or
selected post hoc. Survival and eligibility also collapse with horizon: only
7 continuous streams remained alive through action 8, one through action 16,
and none through action 32. The supported conclusion is therefore narrow:
blind fixed long rollouts from this frontier do not provide sufficient paired
evidence of a useful continuity effect at this scale. Pivot to a different
structural hypothesis rather than adding a long-suffix policy.
