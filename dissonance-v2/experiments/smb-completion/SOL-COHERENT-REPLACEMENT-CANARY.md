# Sol restart: coherent replacement canary

## Scope and frozen evidence

This is a 100-pair mechanism canary, not a campaign, promotion, or completion
claim. It tests one hypothesis: replacing an earlier trace window with a
coherent recorded chunk is more productive than replacing the same window with
the same opaque actions in deterministically shuffled order.

The Sol restart source base is
`ac1e0cae40108b58174ce93b7ea1ce46218c3582`. This file must be committed before
any generated live draw exists; generic implementation work may precede it.

The only source is the recovered C119 artifact on `msr1`:

- Archive: `/root/harmony-smb-goal/dissonance-v2/target/smb-completion/c119-conquest/archive-live.json`,
  11,965,673,612 bytes, SHA-256
  `d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81`.
- Recorded stream SHA-256:
  `ab869286a526dab104f7846ae0313745de7087e3733e99016218defb42e90201`.
  C119 resumed from C118 archive SHA-256
  `415f366092ce23c7e3898a7afe52d677ce2641a743407a16df5661cc3e2e433f`
  using the 3,299-action input SHA-256
  `55df1fe9ec0d4e1819b466a9845f5ff8c6e81d7b7fff17d73f15f230a86967a1`.
- Selected C119 input: archive entry `48076`, parent `29805`, created at
  execution `49709`; semantic input SHA-256
  `584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1`;
  3,297 actions and 155,148 total frames; recorded key begins `(7,0,236)`.
  The live canary reads the newline-terminated compact extraction at
  `target/smb-completion/sol-restart/c119-entry-48076-input.json`, 105,830
  bytes, SHA-256
  `5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159`.
  Its byte and semantic hashes are mandatory runtime checks. The 11.9 GB
  archive hash above is immutable preregistered provenance and is not reread
  or rehashed by each canary execution.
- Its final mechanically observed segment is `[3208,3297)`: 89 opaque actions,
  3,837 frames, nonterminal, reaching `(7,0,236)`. These numbers are integrity
  and outcome context only. The known boundary and segment cost may not steer a
  window, donor, snapshot, action choice, cost comparison, or score.
- Recovered C119 production binary SHA-256:
  `87fb11f300a7af9386eb06c8b55e7a7353d6cb3654b83ee6a5615806e72e2862`.
  The canary's new exact binary hash must also be recorded, and both arms must
  use that one binary.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

The canary seed is `9829488526003250479`, the little-endian `u64` formed from
the first eight bytes of SHA-256(`sol-restart-c119-coherent-replacement-v1`),
whose full digest is
`2fa5334cbc5b6988ea891c37bf0e4f64933b96e6c89f0cd3b585766155b4df32`.

## Frozen paired construction

Replay the selected input from gameplay genesis once and verify the frozen
properties above. Retain bounded deterministic snapshots at action zero and
every 32 actions thereafter. For every candidate restore the nearest recorded
snapshot at or before its recipient, replay the unchanged input to that
recipient, then replay the edited remainder. Treat every
`(buttons, hold_frames)` action as an indivisible opaque value.

The only proposal domain is the generic trailing horizon of 256 actions,
absolute range `[3041,3297)`, derived as `input_length - 256 .. input_length`.
For draw indices 0 through 99 inclusive, the length arm is fixed by
`[4,8,16,32,64][draw_index mod 5]`, giving exactly 20 pairs per arm. Derive
every other integer from
`SHA-256(seed_le || domain || draw_index_le || retry_le || ordinal_le)`, taking
the first eight digest bytes as a little-endian `u64` and reducing modulo the
stated range. Domains are the ASCII strings `donor`, `recipient`, and
`shuffle`. `ordinal` is zero except during Fisher-Yates, where it is the
current upper index.

1. Choose donor and recipient starts modulo `256 - length + 1` within the
   trailing horizon. Deterministically retry until the equal-length windows do
   not overlap.
2. The coherent arm replaces the recipient with the donor actions in their
   recorded order.
3. The control replaces the identical recipient with the identical multiset of
   donor action values, permuted by deterministic Fisher-Yates draws from the
   `shuffle` domain. Retry the construction if the serialized shuffled chunk
   equals the coherent chunk, or if either materialized candidate leaves the
   source input byte-identical. More than 256 retries is an integrity failure.
4. Preserve the exact prefix and tail. Both candidates remain 3,297 actions.
   Restore the same nearest-at-or-before-recipient snapshot for both arms and
   evaluate the coherent arm first, then the shuffled arm.

All 100 recipes are functions only of the frozen input, seed, draw index, and
retry. No outcome may affect a later recipe. A feedback-dependent recipe is
permitted only if it is explicitly implemented by the final hashed binary and
its exact initial state and scheduling rule are committed here before the live
draw; otherwise using one is an integrity failure.

Record for each arm: draw index, retry count, length, donor and recipient
offsets, donor and candidate semantic hashes, action multiset hash, frames from
gameplay genesis, chosen snapshot action and its absolute frame count,
death/failure status, greatest target-reported mechanical tuple reached, and
final deterministic state hash. Candidate absolute frames are the snapshot's
recorded gameplay-genesis frame count plus frames replayed after restore; this
must be exactly equivalent to a from-genesis count.

## Integrity and mandatory stops

Stop immediately, preserve the partial report, and make no search-quality
claim if any of these fail:

- Any compact source bytes, ROM, selected-input identity, action count,
  segment context, baseline outcome, or absolute baseline count of 155,148
  frames mismatches. The historical archive identity was verified during
  recovery and extraction; the live binary must describe it as provenance,
  not claim to have reread it.
- Host entropy, wall time, completion order, an unrecorded seed, or floating
  point reaches candidate construction, evaluation, ordering, or output.
- A pair differs in recipient, length, donor action multiset, or total donor
  hold frames; a candidate changes total action count; an arm does not restore
  the nearest cadence snapshot; an absolute frame count cannot be reconciled
  from gameplay genesis; or fewer or more than exactly 100 valid pairs are
  emitted.
- The report is not canonically ordered by draw index and arm, or any required
  field is absent. Record the report SHA-256.
- Source review finds a literal SMB button choice, route, coordinate/window,
  world/level-specific trigger, image-derived rule, walkthrough/TAS datum, or
  other human/model action knowledge in the generator. Target-reported segment
  boundaries and outcome tuples may be used only as frozen integrity and
  outcome context; the known action-3,208 boundary may not enter proposal,
  snapshot, cost, or scoring logic. Action values may be copied and shuffled
  but never inspected.

There is no routine campaign-stream or archive replay audit. If and only if the
GO condition below is met, rerun this fixed 100-pair canary once with the exact
same binary and seed; its canonical report must be byte-identical before any
larger test is authorized. A mismatch is an integrity stop.

## Frozen decision

An arm is **useful** only when it is nonterminal and either:

- reaches a tuple strictly beyond `(7,0,236)` in no more than 155,148 absolute
  deterministic frames from gameplay genesis; or
- reaches exactly `(7,0,236)` in fewer than 155,148 such absolute frames.

After exactly 100 pairs, GO to a separately preregistered larger test only if
the coherent arm has at least one useful result, has strictly more useful
results than the shuffled arm, and the conditional exact rerun is
byte-identical. Otherwise STOP and record the mechanism as null at this scale.
Pairwise wins under nonterminal, then greater tuple, then fewer absolute frames
at equal tuple are diagnostic only and cannot rescue a failed GO condition.

The canary never promotes code or policy, never continues C119, and never adds
draws after seeing the result.
