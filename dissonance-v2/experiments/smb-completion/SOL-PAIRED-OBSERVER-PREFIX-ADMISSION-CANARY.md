<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol paired observer-prefix admission canary

Status: preregistered before recipe materialization, ROM loading, or candidate
emulation.

## Question

The sealed observer-event prefix salvage canary reconstructed two distinct,
live, probe-surviving normal action boundaries at progress 237 from the exact
C119 progress-236 tip. That established a mechanism: an opaque held action can
pass through a useful target-emitted state which endpoint-only execution loses.
It did not establish that admitting such states improves a search. The known
progress-237 inputs and snapshots are prohibited from this experiment.

This paired mini-campaign asks whether a generic search which admits a bounded
target-emitted strict action prefix through the ordinary archive can select it
later and retain useful descendants. The procedurally work-matched control
applies the same reconstruction and probe rule but does not offer its result to
the archive. Once archives diverge, realized parents, prefixes, and work may
differ. The only arm intervention is prefix admission and its downstream
archive feedback.

This is a structural canary, not a policy promotion or a completion campaign.

## Frozen identities and boundary

- Code base before this experiment: `5a33f3ad`.
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
  `SmbProgressWatermark { world: 7, level: 0, progress: 236 }`, exactly
  155,148 absolute frames. The endpoint mechanical state is
  `(world=7, level=0, progress=236, player_y_bucket=7,
  player_engine_state=8, dead=false, flag_active=false)`.
- Source trace SHA-256:
  `9245f6d42f684a1fcd0a33a762519a51270d1ece2b695ea5a575d83ff64149a1`;
  raw-WRAM SHA-256:
  `936ac08d4c48a2968bec111324fd7ed28628ea89b35baa049b1b5abfffc896ea`;
  `SmbSnapshot` canonical-JSON SHA-256:
  `107bab5a4691ca0e43586b3c95849031782d40f2a3013856161ae4f1d997ae66`.
- The prior observer-prefix report SHA-256
  `ad0bfdfe85b08562b7a76425655c44c82c6f7b0d24259f1c212057662ffb394e`
  is provenance only. The binary must not read that report or any candidate,
  input, snapshot, duration, or button value discovered by it.
- Seed label `sol-restart-c119-observer-prefix-paired-admission-v1`;
  label SHA-256
  `e32f651b50c1958a1005c311bd502b8019b48635390e572b17e4dbbee44568f6`;
  little-endian first-eight-byte master seed `9986100298565103587`.

The source input supplies only an opaque empirical action marginal. Proposal,
parent, and prefix construction never decode button meaning, route,
coordinates, room identity, world, level, progress, or operator goals.
Target-defined archive keys and the frozen `Ord` of final target-reported
watermarks are normal domain adapters and outcome evidence, not proposal
priors; the verdict uses no operator-authored threshold.

Source and ROM reads are bounded at 2 MiB and 16 MiB using maximum plus one
byte. Every count, index, capacity, and frame total uses checked arithmetic.
The standalone binary is `smb-prefix-admission-canary`; its positional
arguments are `<input.json> <create-new-output.jsonl>`, and it reads the ROM
only from `HARMONY_SMB_ROM`. The input is the bounded compact source artifact
above, decoded directly as `SmbInput` JSON; no provenance manifest, archive,
stream, or prior canary report is read.

Source trace validation uses the already-sealed framing. Initialize SHA-256
with ASCII `"smb-trace-canary-v1\0trace\0"`; frame the gameplay-genesis
initial observation as
`length_u64_le || serde_json::to_vec(observation)`; then for each zero-based
source action append its `u64` little-endian index and frame
`serde_json::to_vec(action)` and
`serde_json::to_vec(action_observations)` the same way.

Unless explicitly named otherwise, every typed identity is SHA-256 of the
exact bytes returned by `serde_json::to_vec` for that typed value, with no LF
or wrapper. This includes `SmbInput`, `ButtonChord`, `SmbObservations`,
`SmbMechanicalState`, `SmbSnapshot`, and ordered vectors of them. Raw-WRAM
identity is SHA-256 of the exact 2,048 bytes. Checked conversion to lowercase
hex is presentation only and never re-enters state.

## Frozen recipes

There are eight independent pairs `r = 0..7`, each with 128 draws
`d = 0..127`. Each pair has an observe-only control arm and a prefix-admitting
treatment arm. Before loading the ROM, derive:

```text
pair_digest = SHA-256(
  master_seed_u64_le || ASCII("paired-admission-pair") || r_u64_le)
pair_seed = first_8_bytes_as_little_endian_u64(pair_digest)

action_digest = SHA-256(
  pair_seed_u64_le || ASCII("paired-admission-action") || d_u64_le)
source_index = first_8_bytes_as_little_endian_u64(action_digest) mod 3297

parent_digest = SHA-256(
  pair_seed_u64_le || ASCII("paired-admission-parent") || d_u64_le)
selector_seed = first_8_bytes_as_little_endian_u64(parent_digest)
```

Copy the complete opaque `ButtonChord` at `source_index`. There is no retry,
deduplication, filter, semantic inspection, state association, or outcome
feedback. Serialize the exact ordered vector of tuples
`(r_u64, d_u64, source_index_u64, ButtonChord, selector_seed_u64)` with
`serde_json::to_vec`, with `r` as the outer ascending order and `d` as the
inner ascending order, and record its SHA-256 before ROM loading. Both arms of
a pair use the same ordered 128 `(ButtonChord, selector_seed)` recipes.

## Independent archives and parent choice

Each of the sixteen arms begins from a new archive containing only the exact
C119 source endpoint as entry zero. Reconstruct that endpoint once from
gameplay genesis and validate every derivable frozen identity. For each arm,
directly insert the trusted validated source input, frozen key, milestones, and
snapshot as `id=0`, `parent_id=None`, `created_execution=0`. Origin insertion
runs no viability probe and performs no additional emulation. Require the
resulting archive to contain exactly that one active entry. No historical C119
archive is loaded.

Every arm uses:

- action limit 4,096 and archive limit 257;
- frozen archive key;
- `ProbeAtAdmission45` with masks `[0x00, 0x01, 0x81]` in that order;
- `FewestActions` replacement;
- absent waypoint, no snapback rule, no pinned window, no empirical chord
  update; and
- the existing `ConcentratedRecency` parent selector and its ordinary
  selection/productivity accounting.

At draw `d`, initialize a fresh `libafl_bolts::rands::StdRand` with the frozen
`selector_seed`, then call the real `Archive::select_parent` once with action
limit 4,096. Record the complete selector draw and call the ordinary
`record_selection` and `record_selection_outcome` after admission. Productivity
is true only when this draw newly retains an endpoint or, in treatment, a
prefix; cost is the draw's exact full-action, reconstruction, and probe work.
Using a fresh registered seed per draw prevents archive-dependent RNG
consumption from shifting later recipes. No expandable entry is an integrity
STOP. Treatment may later have additional active entries because admission is
the tested feedback.

Each arm processes its 128 draws serially. The sixteen arms are independent;
host completion order cannot change an archive, recipe, verdict, or report
byte.

## One draw

Restore the selected parent's exact snapshot. Record the action-start
observation, raw WRAM, death/failure state, frame counter, input, milestones,
and snapshot hash. Apply the frozen full action exactly once.

The ordinary full-action endpoint is processed first in both arms. If it ends
alive with `ExitKind::Ok`, snapshot it, run the frozen viability probe, restore
it exactly, and offer the full endpoint to the normal archive as a child of the
selected parent. Its input is `parent.input || full_action`. Duplicate,
probe-refused, rejected, replaced, and newly retained outcomes are recorded.
Death is an experimental result; emulator failure is an integrity STOP.
Both endpoint and prefix insertions produced by draw `d` use
`created_execution = d + 1`; the source uses zero.

Independently of endpoint outcome, inspect the target-emitted observations from
the full action in their stable order. Select at most the first observation
which is strictly after the action start, strictly before its endpoint, and
nonterminal. This is a generic bounded structural rule; it does not inspect
whether the event advanced or regressed. If no such event exists, record none.

For the selected event, derive checked duration
`event.frame_count - action_start.frame_count`; require it to be in
`1..full_action.hold_frames`. Restore the action-start snapshot and apply the
opaque shortened action with the same button byte and that duration. Require
exact equality with the selected event's frame, raw WRAM, decoded mechanical
state, and death flag. Snapshot the reconstructed normal boundary, run the
same frozen viability probe, and restore it exactly. Failure, mismatch, or
non-restoration is an integrity STOP.

Both arms apply and record the identical reconstruction and probe procedure to
their own realized parent state. In
the control, the prefix is observe-only and is never offered to the archive.
In treatment, a live probe-surviving prefix is offered to the normal archive
after the full endpoint, as a sibling child of the selected parent. Its input
is `parent.input || shortened_action`; its key, milestones, duplicate check,
cell capacity, and replacement decision use the same ordinary archive path as
the endpoint. The continuing full action never becomes the prefix's child.

An archive insertion which returns an existing input is a duplicate, not a
new retention. A structural-prefix admission is a newly allocated treatment
entry created by this prefix path. Record its input and full-snapshot hashes.

If a structural-prefix entry is selected at a later draw, mark it selected.
Only a newly allocated ordinary full-action endpoint produced directly from
that selection counts as its retained descendant; another emitted prefix does
not. Record descendant input/snapshot hashes and whether its retained key
watermark is strictly beyond the selected structural prefix's key watermark.
Replacements do not erase this recorded causal lineage.

For the paired directional comparison, each arm's final maximum considers
only active entry zero and active entries created by ordinary full-action
endpoint admission. Prefix-only entries never directly raise this maximum.

## Parallel execution and work

Use exactly twelve persistent workers. Arm ordinal is `2*r` for control and
`2*r+1` for treatment; assign ordinal modulo twelve. A worker initializes one
target, receives the validated source snapshot, and executes its assigned arms
in ascending ordinal. Each reply carries its arm ordinal and an inner
success/error. The coordinator buffers every reply and consumes results or
raises errors in ascending arm ordinal; only it writes the report.
Worker initialization is retained as `Ready(SmbTarget)` or `Failed(String)`.
Every assigned arm always returns an outer-success reply containing its ordinal
and inner result, including when initialization failed. Missing, duplicate, or
wrong-worker ordinals are integrity STOPs, so host scheduling cannot choose
which error becomes canonical.

The hard deterministic upper bound is:

- 2,048 full actions: 245,760 frames;
- endpoint probes: 276,480 frames;
- 2,048 at-most-one prefix reconstructions: 243,712 frames;
- prefix probes: 276,480 frames;
- one source replay: 155,148 frames; and
- thirteen target setups at the sealed 361 frames each: 4,693 frames.

Total hard bound: 1,202,273 frames. Every component and realized total is
recorded separately. Crossing a bound is an integrity STOP. Wall time is not
recorded and never controls stopping.

## Canonical evidence

The create-new NDJSON order is header, source baseline, frozen recipes, then
arm records in ascending ordinal, followed by paired classification and
summary. Per draw record selected parent identity, recipe, full endpoint and
prefix evidence, all probe attempts, archive outcomes, work, retained
lineage, full state/input hashes, and active maximum after admission.

Recompute all summaries from evidence. All eight preregistered pairs enter the
verdict even if two complete pair recipe vectors happen to be equal.
Deterministic executions from an identical parent snapshot and action must
have byte-equal pure target evidence: full-action observations/outcome,
selected event, prefix reconstruction, probe transcript, snapshots, and local
work deltas. Arm policy, archive admission/occupancy/ids/lineage/maxima,
constructed full-input identity, and cumulative bookkeeping are excluded;
those may legitimately differ with archive contents or parent input. Any
remaining mismatch is an integrity STOP. Exact candidate hashes are
deduplicated only for the structural gate's explicit distinctness counts.

`body_sha256` covers exact UTF-8 NDJSON bytes from header through the last
pre-summary record, including every LF. After summary and LF, flush and sync,
then print the complete file SHA-256. The report contains no wall-clock field.

## Frozen decision

For each pair, compare the final maximum `SmbProgressWatermark` among active
entry zero and active ordinary full-action endpoint entries. Transient
observations and prefix entries never count directly. A treatment win is
strictly greater than control; a control win is the reverse; equal maxima tie.
Let `w` and `l` be treatment and control wins among the eight pairs and
`n = w + l`. Ties remain recorded non-wins; the exact conditional sign test
omits them from `n` and necessarily requires at least five treatment wins when
there are no control wins.

Compute the exact one-sided sign tail
`sum(k=w..n, binomial(n,k)) / 2^n` with checked integer arithmetic. The
directional pair gate passes only when `w > l` and
`20 * sum(k=w..n, binomial(n,k)) <= 2^n`.

The structural-chain gate passes only when all of these hold:

1. at least two distinct structural-prefix snapshot hashes from at least two
   treatment pairs were newly admitted normally;
2. at least two such distinct structural-prefix entries from at least two
   treatment pairs were selected on later draws;
3. those selections produced at least two distinct newly retained ordinary
   full-action endpoint descendant snapshot hashes;
4. at least one such endpoint descendant has a retained key watermark strictly
   beyond its selected structural prefix; and
5. at least one pair's treatment-winning final maximum is attained by an
   active ordinary endpoint counted as a direct descendant under condition 3.

All fixed draws run even if an observation appears terminal; this canary has
no outcome-dependent early stop and defines no separate completion verdict.
**GO** requires both the directional pair gate and structural-chain gate. GO
authorizes a separately reviewed integration of bounded target-emitted action
prefixes into the generic searcher and a larger paired campaign; it does not
authorize importing any post-hoc candidate from this canary.

If there are zero structural-prefix admissions, zero later structural-prefix
selections, or zero beyond-prefix retained descendants, the verdict is
**STOP**. Every other non-GO result is **INCONCLUSIVE**. Neither STOP nor
INCONCLUSIVE authorizes enlargement, altered recipes, selected durations, or
post-hoc candidate adoption.

There is one registered live run, no routine replay audit, and no automatic
rerun. The temporary runner is removed after recording the result. Production
searcher code changes only after a GO.

Based on the prior `msr1` canaries, expected live time is approximately 6–8
minutes if prefix/probe sparsity resembles the salvage GO. The hard-work
projection is approximately 10–11 minutes; allow 12–15 minutes operationally.
Neither estimate is a stopping condition.
