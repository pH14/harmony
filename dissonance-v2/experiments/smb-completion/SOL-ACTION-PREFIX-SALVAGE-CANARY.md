<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol observer-event prefix salvage canary

Status: preregistered before candidate emulation.

## Question

C119 observes mechanical progress during a held input but snapshots and offers
a candidate to the archive only after the whole input ends. Its report reached
an aggregate progress watermark of 248 while the selected live tip ended at
236. In the sealed rollout census, 8 of the 100 first opaque actions
transiently crossed beyond 236 but did not end at a live deeper action boundary.
Those already-inspected full-action outcomes motivate this canary; no shortened
candidate at an interior observer event, nor an admission probe from one, has
been executed or inspected.

This census asks whether ending an unchanged opaque action at a mechanical
observer event can expose a live, probe-surviving deeper action boundary which
the full hold passes through and loses. It is an existence/mechanism test, not
a throughput comparison, archive-admission test, duration-policy comparison,
or policy promotion.

## Frozen identities

- Code base before this experiment: `91919a81`.
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
- Sealed source replay evidence: trace SHA-256
  `9245f6d42f684a1fcd0a33a762519a51270d1ece2b695ea5a575d83ff64149a1`,
  final raw-WRAM SHA-256
  `936ac08d4c48a2968bec111324fd7ed28628ea89b35baa049b1b5abfffc896ea`,
  and final `SmbSnapshot` canonical-JSON SHA-256
  `107bab5a4691ca0e43586b3c95849031782d40f2a3013856161ae4f1d997ae66`.
- Reused rollout seed label `sol-restart-c119-continuous-rollout-v1`,
  SHA-256
  `d0a86c80cac50cec33f1a6a55db713f93468796f14f554cdcc040f5d000a9d60`;
  master seed `0xec0cc5ca806ca8d0` (`17009187366200191184`).
- Sealed complete 100-by-32 rollout action recipe SHA-256:
  `a000be41bdc99f2b7c0fd1b35d8a35770c0f377b446d17ea724735d8454800a2`.

The executable and configuration hashes are recorded before outcomes. Source,
ROM, and executable reads are bounded at 2 MiB, 16 MiB, and 256 MiB using
maximum plus one byte. All counts, indices, frame totals, and capacities use
checked arithmetic.

## Frozen recipes

Reconstruct the complete sealed rollout corpus before loading the ROM. For
each stream `s = 0..99` and action `k = 0..31`, derive

`digest = SHA-256(master_seed_u64_le || ASCII("rollout-corpus-index") ||
s_u64_le || k_u64_le)`.

Interpret the first eight digest bytes as little-endian `u64`, reduce modulo
3,297, and copy the complete opaque `ButtonChord` at that index from the frozen
source input. Require SHA-256 of
`serde_json::to_vec(Vec<Vec<ButtonChord>>)`—the ordered 100 action vectors and
no metadata—to equal the sealed action-recipe hash above. Re-derived source
indices are recorded separately and are not part of that frozen hash.

For each stream use only its already-frozen `k=0` chord
`A_s = (buttons_s, H_s)`. No stream is omitted. Serialize the ordered vector of
`(stream_u64, source_index_u64, A_s)` with `serde_json::to_vec` and record its
SHA-256 before loading the ROM as this canary's recipe hash. There is no retry,
filter, state association, outcome feedback, or semantic inspection of buttons
or duration. This is the same explicit artifact-derived empirical prior as the
sealed rollout census, not operator-authored Mario knowledge.

Define `full_chord_sha256` as SHA-256 of `serde_json::to_vec(A_s)`. Insert
these identities into a `BTreeMap` in ascending stream order; the lowest stream
is the canonical owner of an equal chord. Deduplication is outcome-independent
and used only by the gate. Every duplicate chord is still executed; normalized
full-action, candidate, probe, and classification evidence must be byte-equal
across its group. Stream/source-index identity and instance-cumulative work are
excluded from that comparison, as are coordinator-only canonical-owner and
gate-bookkeeping fields. Any other difference is an integrity STOP.

Different full chords can also reconstruct the same shortened candidate, for
example when their button bytes match and both contain the same event offset.
Group exact `candidate_sha256` values independently. Normalized candidate and
probe evidence must be byte-equal within that group, excluding stream,
full-chord/source-index identity, canonical-owner bookkeeping, and
instance-cumulative work. Full-action outcome and stream classification are
not compared across this second group because the remaining holds differ. Any
other candidate-group difference is an integrity STOP.

## Exact execution

The standalone binary is `smb-observer-prefix-salvage-canary`; its positional
arguments are `<input.json> <create-new-output.jsonl>`, and it reads the ROM
only from `HARMONY_SMB_ROM`.

Replay the 3,297-action source once from gameplay genesis. Validate the
derivable source byte, semantic, action-count, outcome, frame, trace, WRAM,
snapshot, ROM, and current-executable hashes. Record the C119 archive, stream,
entry, parent, creation execution, and recovered production-binary identities
as frozen provenance constants without reading those large historical
artifacts. Retain the exact endpoint snapshot and trace hasher. The trace
framing is the sealed first-canary framing: initialize
SHA-256 with ASCII `"smb-trace-canary-v1\0trace\0"`; frame the gameplay-genesis
initial observation as `length_u64_le || serde_json::to_vec(observation)`;
then for each zero-based source action append its `u64` little-endian index and
frame `serde_json::to_vec(action)` and
`serde_json::to_vec(action_observations)` the same way.

Use exactly 12 persistent workers. Stream `s` is assigned to worker `s mod 12`;
each worker constructs one identical target, and every stream begins by
restoring the same registered C119 snapshot. A worker executes its streams in
ascending order. Every reply carries its stream and an inner success/error;
the coordinator buffers all replies and consumes successes or raises errors in
ascending stream order. Only the coordinator writes the report. Initialization
failures are retained as worker state and returned for each assigned stream.
Host completion order, scheduling, and instance-cumulative work cannot affect
recipes, classification, canonical bytes, or error choice.

For each stream, first restore the endpoint and execute the full `A_s`. Record
the complete ordered action observations, requested/actual/absolute frames,
death/failure, endpoint and maximum watermark, raw WRAM, full snapshot, and
candidate trace hashes. `ExitKind` other than `Ok` is an integrity STOP. Death
is an experimental outcome. The restored observation must be the registered
live frame 155,148; absolute frames must equal 155,148 plus frames since
restore; and the observation delta must equal the independently delimited
`frames_clocked()` work delta.

From that full action, enumerate only observations which are all of:

1. strictly after frame 155,148 and strictly before the full action endpoint;
2. alive and strictly beyond the registered baseline watermark; and
3. the first occurrence at their exact frame offset.

For each such observation in ascending frame offset, derive checked duration
`d = observation.frame_count - 155,148`, require `1 <= d < H_s`, restore the
source snapshot, and execute the shortened opaque action
`A_s,d = ButtonChord::new(buttons_s, d)`. Require the shortened endpoint frame,
raw WRAM, decoded mechanical state, and death flag to equal the full action's
recorded observation at offset `d`; a mismatch is an integrity STOP. This
reconstructs the same mechanical state as a normal action boundary, with the
controller released by the existing target action interface.

For each reconstructed candidate record:

- exact event offset, full and shortened actions, full-chord hash, and the
  matched full-action observation;
- `candidate_sha256 = SHA-256(serde_json::to_vec(SmbInput {
  actions: source.actions || [A_s,d] }))` and
  `suffix_sha256 = SHA-256(serde_json::to_vec([A_s,d]))`;
- endpoint/max watermark; requested, actual, absolute, since-restore, and work
  frames; and raw WRAM, full snapshot canonical-JSON, and trace hashes; and
- an ordered nested vector of viability-probe attempts.

The candidate trace clones the source trace, appends logical action index 3,297
as little-endian `u64`, and frames the exact shortened action and its action
observations. From each candidate snapshot, run the exact C119 45-frame
admission viability probe: restore before each mask, try
`[0x00, 0x01, 0x81]` in order, and stop at the first mask surviving all 45
frames. Each nested attempt records mask, exact work frames, death, failure,
and survival. A probe `ExitKind` other than `Ok` is an integrity STOP; death is
ordinary non-survival. Restore the candidate after probing.

Per stream there are at most 119 candidate offsets. Truncated candidate replay
is capped at 714,000 requested frames; probe work is capped at 1,606,500
frames; full-action work is capped at 12,000 frames. Crossing a cap is an
integrity STOP. Instance setup and the single source replay are reported
separately. Checked arithmetic derives all exact realized totals.

The create-new canonical NDJSON order is header, baseline, recipes, then one
nested evidence record per stream in ascending order, followed by classification
and summary. It records all artifact, configuration, executable, recipe,
evidence, and work hashes/counters. `body_sha256` is SHA-256 over the exact
UTF-8 NDJSON bytes from header through the final pre-summary record, including
every LF. After writing summary and LF, flush and sync, then print the complete
file SHA-256 separately. There is no wall-clock field, archive mutation,
routine replay audit, or automatic enlargement.

## Classification

All progress comparisons use the derived lexicographic `Ord` of
`SmbProgressWatermark { world, level, progress }`.

For each stream:

- `atomic_live_deeper`: the full action ends alive beyond baseline;
- `atomic_transient_deeper`: a recorded interior observer event is alive and
  beyond baseline;
- `event_prefix_salvage`: the full action is not `atomic_live_deeper` and has at
  least one exactly reconstructed interior live-deeper observer event; and
- `probe_surviving_salvage`: `event_prefix_salvage` and at least one such
  candidate survives the frozen viability probe. Its credited evidence is the
  smallest event offset which survives.

Candidate identity is the exact full-input hash at the credited offset. The
full snapshot hash identifies the complete reconstructed target state. A later
full-action regression or death does not erase the earlier independently
reconstructed action boundary.

Summary counts are recomputed from evidence records. Report raw and canonical
counts for atomic live-deeper, atomic transient-deeper, event-prefix salvage,
probe-surviving salvage, and probe refusal; distinct full-chord, candidate,
and snapshot hashes; ordered credited evidence; total full-action, shortened
candidate, and probe work; and requested frames. Do not compare this
conditional census to endpoint-only execution as a rate estimate.

## Frozen decision

GO only if canonical probe-surviving salvages contain at least two distinct
`full_chord_sha256` values, two distinct credited `candidate_sha256` values,
and two distinct credited full `SmbSnapshot` hashes. One canonical
probe-surviving salvage, or any nonempty canonical salvage set which fails one
of the three GO distinctness thresholds, is INCONCLUSIVE. Zero is STOP,
including when exact event-prefix candidates exist but all are probe-refused.

GO establishes only that the existing endpoint-only action interface loses
multiple probe-surviving target states at this fixed source. It authorizes a
separately preregistered, small paired campaign canary in which a generic target
may expose opaque interior observations as candidate action prefixes and the
searcher prices reconstruction work plus normal archive admission against
endpoint-only execution. It does not authorize hard-coding any observed
duration, button, coordinate, route, source state, or candidate, and it does
not import a post-hoc candidate into the campaign.

INCONCLUSIVE or STOP forbids enlarging or reinterpreting this census. The next
registered hypothesis may test the neutral one-generation admission bridge or
compact short-job lineage, but may not rescue this gate with selected chords.
There is one registered live run and no automatic exact rerun.

## Sealed result

Status: **GO**. The single registered run completed on `msr1`; there was no
rerun and no candidate was imported into a campaign.

- Implementation commit: `d83512444176e647f52a9cdd3bae05d8223a468e`.
- Temporary implementation source SHA-256:
  `7221f9566902b76a64ee5d32da058073436b7adc7e9a343b3d81a0c3d5726b32`.
- Stable commit-rooted source archive SHA-256:
  `188d8fa40e6cb3ca74d227ddcfc278083bd4d83747450d8e288f36a9f6980b2d`.
- Release executable SHA-256:
  `675dd8e80a716b391311ec236a0b40ba6b6cca203f7b24c6124bad339ffe4e2f`.
- Config SHA-256:
  `b004cd9cce1524ef8b9ddea77a6345ecaf1f2c0ab2f8d317cba1a4bf9b7b1b7e`.
- Canary recipe SHA-256:
  `0559afd3066ad058d3611fd71fcf58b7dcb86a5b62dba743cef0813815e7a2e2`;
  the reconstructed 100-by-32 action hash exactly matched the sealed
  `a000be41bdc99f2b7c0fd1b35d8a35770c0f377b446d17ea724735d8454800a2`
  identity.
- Canonical report:
  `/root/harmony-smb-sol-observer-salvage-d8351244/results/c119-observer-prefix-salvage-100.jsonl`;
  105 lines, 3,703,680 bytes; body SHA-256
  `9f4e90a91de0ef966603cb2064123c7795c38e9d50e6ac15b5390c53747ca133`;
  whole-file SHA-256
  `ad0bfdfe85b08562b7a76425655c44c82c6f7b0d24259f1c212057662ffb394e`.

Raw evidence contained 15 full actions ending live-deeper, 23 with an interior
live-deeper observer event, 8 event-prefix salvages, 7 probe-surviving
salvages, and 1 probe refusal. After the frozen equal-full-chord
deduplication, the corresponding counts were 14, 21, 7, 6, and 1.

The six canonical credited opportunities reduced to two distinct candidate
inputs and two distinct full target snapshots, satisfying every frozen GO
threshold:

- offset 19 frames, candidate SHA-256
  `9ae15f068d9b2dea7bd2b9fec20cd7883b4de315d8c560f09b905272956e0950`,
  snapshot SHA-256
  `8cec62febe6c6730ee59e0275faee594f85289219cab467abf47bf7a1e84190e`;
  and
- offset 23 frames, candidate SHA-256
  `6c4a57677b162c0666b6b4a6896fe978c4200fca1a3f18974f89ff3e93cf1999`,
  snapshot SHA-256
  `d2b7bf86277bfe131b5174cecbbe7a4b80b7af0b6e07ae91224aa81d86bf7361`.

Both are live action-boundary states at
`SmbProgressWatermark { world: 7, level: 0, progress: 237 }`, one bucket beyond
the registered C119 source. They demonstrate a repeated endpoint-only loss,
not a new historical maximum: C119 had already observed deeper transient
watermarks elsewhere. The result authorizes only the preregistered next step
described above—a small paired normal-admission campaign canary for generic
interior candidate-prefix emission.

Measured work was 4,701 full-action frames, 11,288 candidate-reconstruction
frames, 10,809 probe frames, 4,693 target-setup frames, and the single 155,148
frame source replay. Every independent body, file, recipe, executable, source,
ROM, trace, WRAM, snapshot, and setup-work identity reconciled.
