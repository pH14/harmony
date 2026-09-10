<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Metroid workload

This package ports the native Metroid experiments to the refactored campaign
contracts. Execution, controller interpretation, RAM decoding, setup, and
outcome evidence stay in `nes-workload`; archive search and route reuse stay in
`searcher`. ROMs and core binaries are private local inputs.

The registered workload starts a new game through ordinary power-on menus.
Equipment, tanks, boss defeat flags, and the ending flag come from cartridge
work RAM. The decoder maps Samus's screen using name-table membership and scroll
direction, so camera coordinates do not masquerade as player coordinates. All
source addresses and meanings are documented beside their constants in
`target.rs`. Zero health is death; the ending flag is victory. The terminal
identifier is `death_or_ending_v2`, correcting the prototype's stale
`death_only_v1` label without changing that prototype's predicate.

The opt-in `metroid_terminal: death_or_bcd_underflow_or_ending_v3` evaluation
policy also marks decoded health >=8000 as terminal. The damage routine stores
a BCD subtraction before testing borrow and clearing lethal damage; a frame
boundary can expose this intermediate value. A verified development endpoint
reported 9800 and died under every one-frame controller mask. Raw health remains
unchanged for replay inspection. Historical v2 is still the default; stream
headers distinguish the policies and reject a mismatched replay context.
This correction is independent of the optional archive-key refinement.

`archive.rs` records the experimental adapter policy explicitly. It pools
16-pixel positions through 32-pixel cells, 128-pixel regions, map cells, and
inventory counts. Posture and door-transition state distinguish possible
continuations. Health and missile stock are same-slot preference, not extra
spatial slots. Item and tank counts describe discovered capabilities; no
particular item, room, door target, or route is supplied. Coverage and pickup
counters are reporting evidence across explored branches, not proof that one
trajectory achieved their union. The inherited count representation and
lexicographic resource preference are policy tradeoffs, not true capability
or resource dominance.

The legacy primary progress watermark records equipment bit count **plus boss
defeats**, and missile capacity.
The `milestones.tanks` field combines missile capacity divided by five
and energy tanks (boss capacity bonuses also inflate that legacy field), so it
can improve while the primary watermark stays fixed.
`milestones.areas` is an area bitset, not a count: decimal 3 has two area bits
set. Inspect the individual fields and the verified witness before calling a
run stalled or combining observations into one trajectory.

The live sidecar separately counts observed map cells with a fixed 32 KiB bitmap
over raw area identity and the 32x32 map coordinates. It includes living gameplay
observations across explored branches in this run, and does not claim that one
trajectory visited their union. This observation memory is outside the archive's
logical budget and included in process RSS. The counter never enters archive
keys, input selection, rewards, or deterministic reports; it remains cumulative
when the archive's novelty ledger is compacted.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
for paired search comparisons and full small-campaign replay. `metroid-campaign`
also exposes the native experiment command. The source lineage is documented in
[the synthesis record](../../../../benchmarks/search/SYNTHESIS.md).

## Named milestone evaluation

`workload_diagnostics.named_progress` reports Morph Ball, Bombs, Long Beam,
High Jump, Screw Attack, Varia Suit, Wave Beam, and Ice Beam independently;
Brinstar, Norfair, Kraid's area, Ridley's area, and Tourian independently; and
Kraid defeated, Ridley defeated, Mother Brain defeated, escape started, and
the ending independently. Mother Brain initialization is not defeat; its $98
state machine is interpreted only in Tourian gameplay. Brief defeat/escape
transitions are latched from each frame without adding search events. Area entry never
implies boss defeat. Equipment is a union of observed gear bits, so beams lost
or replaced later remain recorded. `max_missile_capacity` and `max_energy_tanks`
are separate maxima, not a combined pickup score. Capacity is not a pickup
count: a boss defeat grants 75 additional missiles of capacity.

Every named milestone has a `first_seen` entry. Null means not observed within
this run's scope and budget. `execution` is the first admitted execution that
observed it; `route_action_end_frame` is the action endpoint on that discovered
input, **not** cumulative emulator work. Cartridge RAM is read after a held
controller action, so this is not an exact within-action pickup timestamp.
The finite vocabulary bounds observer memory and the number of saved tapes.
These fields do not change keys, rewards, input draws or champion ordering.
The common evaluator can opt into `stop_after_milestone` with one existing name,
such as `energy_tank`. That criterion stops new campaign reservations after its
first admitted observation and preserves normal drain, export and replay. Its
observation-policy version is recorded; the default remains a full-budget run.
The resulting `first_milestone` reports cumulative admitted work through the
event's job, distinct from the route coordinate in `first_seen`. A milestone
first observed beyond the frame cap is retained as evidence but is not a
budgeted endpoint hit. No item order, route or action policy is supplied.

The common runner saves each first-discovery input under `milestone-inputs/`
and replays it on two independent targets, checking both the named milestone
and the deterministic endpoint. Its main champion/victory witness separately
reports one trajectory's named progress. Do not call a union over search branches
one successful playthrough. Discovery-tape verification is charged to the
verification phase; the bounded export cost during discovery is part of search.
A first named discovery reconstructs its tape regardless of output configuration,
so publication options cannot alter deterministic report counters.
The live observer includes only observations admitted in this run, not a restored
archive's complete history. Missing fields in older reports mean **unavailable**,
not zero. A retained champion replay cannot establish everything an older search
explored or prove that no other branch defeated a boss.

### Experimental endpoint encounters

Build with `--features metroid-boss-context-audit` to report snapshot-local boss
context at completed, live action endpoints. Intermediate WRAM observations are
paired with endpoint cartridge RAM, so they are deliberately ineligible. Empty,
failed and early-dead actions supply no eligible sample. Origin/restored markers
are not counted as newly executed actions. The report distinguishes admitted
actions, eligible live endpoints and classified endpoints; it includes the first
classified endpoint's admitted execution, route frame, area and enemy-slot mask.
Counters merge before retention and do not claim fight capability, exact damage,
defeat, retention history or encounters between action endpoints.

`nes-eval` writes `first-endpoint-encounter.json` with the first endpoint and its
producing searched input, then replays that input twice and requires the same
live encounter at its end. `result.json.endpoint_encounter_witness` records the
artifact hash, replay and `known_replay_frames`; charge those frames in addition
to champion and named-milestone replay costs. No artifact means no positive was
recorded; use the diagnostic counts to distinguish missing eligible observations.
A first encounter reconstructs its input even with publication disabled, keeping
deterministic reconstruction counters independent of output configuration.

The feature is off by default. It adds constant-size metadata to observations
and snapshots, charged by the existing snapshot memory accounting, and records
`endpoint_encounter_observation: metroid-live-endpoint-boss-slots-v1`. Its stream
and checkpoint use `endpoint-context-v5`; semantic result digests use
`endpoint-context-v6`. It changes no keys, rewards, action law or selection rule,
but metadata and first-input reconstruction have resource costs. Instrument both
arms identically; compatibility at bounded fixtures does not establish neutrality
near memory or wall limits. Positive native fight qualification is still required
before stronger claims; see the [observation contract](../../../../benchmarks/search/depth-transfer/endpoint-encounter-contract.md).

The names and boss flags follow
[`Metroid_Defines.asm`](https://github.com/nmikstas/metroid-disassembly/blob/4270d57f9468daebdeea485686e31e26218a780c/Source_Files/Metroid_Defines.asm),
with the defeat write in `Bank07.asm` at `LDD75`: `(InArea & 0x0f) >> 1`
stores 1 at $687B for Kraid and 2 at $687C for Ridley. The previous decoder
incorrectly tested bit 0 for both bosses. Correcting the count is versioned as
key policy v8; named boss observation bytes require stream/checkpoint/result
digest v4 (v2 introduced named Kraid/Ridley flags; v3 added Mother Brain state;
v4 latches transient Tourian events). Named-progress v2 and replay-probe v2 also
correct origin/retrospective route timestamps to exclude genesis setup; the probe
reports physical work and setup separately. Earlier v1 timestamps in the 007
audit are superseded by the corrected audit, not silently rewritten.
Existing v7 results and the v2 reporting measurements remain immutable. The unrelated legacy combined
capacity score remains explicit rather than silently redefining past policies.

Retrospective replay, without submitting an existing solution to search:

```sh
cargo build --release --manifest-path workloads/nes/Cargo.toml --bin nes-progress
workloads/nes/target/release/nes-progress metroid CORE ROM searched-input.json > progress.json
```

The tool starts from ordinary new-game genesis, records named discoveries,
repeats the replay, and includes ROM/core/input hashes. In its replay report,
`first_seen.execution` counts tape actions, not historical search executions.
For Mega Man 2, `nes-progress mm2 CORE ROM searched-power-on-prefix.json wily4`
checks that the retained tape still reaches that stage through the current
runtime; it is a compatibility fixture, not a fresh search result.

The opt-in `retention_audit` evaluation flag records at most 16 competing input
pairs in each of five diagnostic categories: different equipment, different
capacity, resource tradeoffs, equal preference, and ordered resources. A separate
sampling hash leaves the campaign RNG untouched. Incumbents with evicted cached
snapshots are counted but excluded, so samples are not a census of every state.
The declared 2.5 MiB action-payload bound plus metadata/reconstruction and output
buffers are diagnostic overhead reflected in RSS and I/O, outside logical archive
memory. `metroid-retention-probe` reconstructs each pair and applies the same
sampled suffixes to both sides. It reports physical probe/prefix frames, gains,
survival, and living map exits separately. These diagnostic starts never count
as fresh validation; differing endpoints alone are not useful-future evidence.

Stored action vectors are compacted before sampling retains them. A suffix
append can otherwise leave spare vector capacity above the input-length limit;
the declared payload bound covers retained capacity, not only live elements.
Temporary reconstruction and serialization buffers remain additional overhead.

The optional Cargo feature `metroid-complete-retention-audit` writes audit
`v2-local-survivors`. Each sampled pair can include every incumbent and the local
rule's proposed keep/remove flags. The callback precedes global population and
memory eviction; these records do not certify the final globally active set.
Missing snapshots, unsupported slot sizes and oversized extra inputs leave an
explicit absent complete record and increment diagnostic counters. At most two
incumbents are materialized, with the same8192-action input limit. The action
payload bound becomes5MiB; reconstruction and output buffers remain additional
diagnostic overhead. Sampling indices, observations, search counters and RNG are
unchanged. Default builds preserve the v1 audit. Comparing a discarded state's
suffix outcomes with the union of all local survivors avoids mistaking novelty
against one incumbent for a future lost from the complete set.

The probe accepts `CORE ROM AUDIT OUT TRIALS ACTIONS [TERMINAL_POLICY [SEED]]`.
Omitting the final arguments keeps the historical terminal predicate and suffix
seed. Its v2 report records the terminal identity, numeric seed, and exact suffix
hash. Use terminal v3 for audits from corrected campaigns; living-map checks
respect that predicate. A separately registered seed permits new suffix probes
without silently reusing the original development suffix bank.

An optional final `PREFIX_LIMIT` argument records cumulative outcomes after
each of the first requested actions in `prefixes.jsonl`, while executing the
same full suffix bank. `prefix-metadata.json` records the horizons and bank hash.
After an early terminal, later horizons repeat its outcome and frame count.
These counters observe the single run; summing them would double-count physical
work. Full-horizon outputs retain their historical format and default calls
produce no prefix sidecar. This permits checking the actual one-to-six-action
job horizon without replacing the frozen 24-action diagnostic bank.

The opt-in Cargo feature `metroid-refined-archive` builds a separate experimental
v9 key policy. It uses 8-pixel retention positions and raw pose, while preserving
the existing 32-pixel selection cells, 128-pixel regions, map groups, progress
ordering, resource preference and one representative per retention slot. The
default build retains v8 semantics and serialization. Streams record distinct
key-policy identifiers and reject replay under the other build's policy.
The key's serialized field layout is unchanged; the recorded policy identifies
the position/pose interpretation. Archive imports already re-derive keys from
reconstructed states rather than copying the old key identity.

This is an abstraction experiment, not a validated improvement. Separating
sampled competitors with different futures motivates testing a key but does not
prove better discovery. More slots consume the same archive byte budget, and
retained alternatives may still receive too little work. Actual-ROM replay and
matched fresh-search results, including failed escalation gates, are recorded
in the [research ledger](../../../../benchmarks/search/retention-theory/README.md).

## Standalone boss-memory diagnostics

`metroid-boss-probe CORE ROM INPUT.json OUT` reads raw loader and enemy-slot
bytes while replaying an existing searched tape. It compares ordinary chords
with two independent one-frame replays, requiring equal final emulator bytes
and decoded state plus identical frame-trace hashes. Inputs are bounded at
8,192 actions/250k route frames; diagnostic trace output is capped at 32 MiB.
No probe value changes search observations, snapshots, retention or termination.
The standalone probe uses corrected terminal v3 and stops on a terminal tape.

The raw addresses come from the pinned
[disassembly](https://github.com/nmikstas/metroid-disassembly/tree/4270d57f9468daebdeea485686e31e26218a780c):
`Metroid_Defines.asm` and Bank07 loader, slot, HP and hit routines. A special
byte can change during combat or remain in an inactive slot. The report keeps
loader, slot status, tag and HP separate; it does not infer damage totals,
encounter identity or defeat from them. An empty existing-route trace is only
a negative control, not evidence about every route in its source campaign.

Append `boss-area` to retain every frame in either boss area, including hit
states that clear the loader flag and overwrite the special tag. This opt-in
output uses probe format v2 and `boss_area_all_frames_v1`; the four-argument
invocation retains its original format and filter. Both modes hash all sampled
frames and keep the same fixed input/output bounds. Full-area retention may
reach the output cap sooner and fails explicitly instead of truncating evidence.
The [positive-control study](../../../../benchmarks/search/depth-transfer/README.md)
found 98 real HP decreases missed by both instantaneous guards. These raw traces
support subsequent enemy-lifetime analysis, not an automatic damage counter or
identity across snapshot restores. The mode affects only this standalone probe.

## Retrospective motion descriptors

`metroid-kinematics-probe CORE ROM AUDIT.json OUT` reconstructs at most sixteen
recorded replacement pairs and reads facing, signed-speed bytes and four motion
accumulators from the same pinned disassembly. Each endpoint is replayed twice,
must match its recorded mechanical state, and must retain identical snapshot
bytes before and after the read. The complete run is capped at 2M physical
frames; each input is bounded at 8,192 actions and 250k route frames.
`pairs.jsonl` retains completed per-pair evidence if a later check fails.

The probe uses terminal v3 and runs no suffix trials or fresh search. No field
is added to search observations, snapshots, keys or policies. Comparing these
descriptors with existing continuation outcomes can refute a proposed grouping;
it cannot establish that motion alone causes the difference or that keeping
its groups improves discovery. The coarse descriptor and design gate are
registered in the [P04 ledger](../../../../benchmarks/search/retention-theory/README.md).

The optional `metroid-motion-context` feature adds `motion_context: Option<u16>`
to the archive key. Actual candidate and reconstructed-origin keys derive it
from the target's already cached endpoint RAM; numeric-only key construction
leaves it absent. It encodes raw facing and the signs of both signed speeds,
without preferring a direction in the retention rule. Group identity, opaque quality, observations
and emulator snapshots remain unchanged. Both arms of a motion-retention
comparison use this same metadata and memory accounting.

The feature records key policy v10 with the ordinary 16-pixel geometry, or v11
if combined with the separate refined-archive feature, and result-digest
`metroid-semantic-postcard-1.1.3-sha256-hex-motion-v5`. The combined variant is
defined for compatibility but is not qualified by the motion experiment.
Feature-disabled builds retain their old key layout and digest identifier.
`context_representatives_2_v1` uses only context equality and keeps at most two
quality-ranked contexts; `quality_representatives_2_v1` is its capacity control.
The motion probe additionally checks cached context against direct RAM and,
when feature-enabled, checks the actual campaign key, without running search.

R04 qualifies alphabet-only fresh search. The complete key's deterministic
ordering includes the new metadata; generic splice-donor and resume comparisons
that use that ordering can therefore change when the feature is enabled.
Splice and resume behavior require separate qualification. Matching the feature
in both R04 arms holds that metadata constant in the policy comparison.
The existing [ordering-bias follow-up](https://github.com/pH14/harmony/issues/270)
tracks the full-key donor-selection limitation; do not treat motion retention's
alphabet-only qualification as resolving that issue.

### Snapshot-local interval mode

Append `boss-context` for format v3, `boss_context_intervals_v1`. This retains
full-area rows and adds the six raw saved-status bytes at `$040C + slot` and
six optional interval reports. Normal states use current boss attributes;
hit/death states use the saved prior attributes. Comparisons require consecutive
frames, the same epoch and visible identity, and usable HP. Reports preserve
unavailable values separately from comparable zero change. The HP-loss sum is
null when no interval is comparable.

`boss-context-restores` qualifies the integration by restoring the current
snapshot every 4,096 route frames in each one-frame pass. It verifies identical
raw bytes across each restore, clears the observer, and reads a fresh baseline
before advancing. This mode records the restore count and adds no route frames.
Both context modes keep the same input and 32 MiB trace bounds; legacy default
and `boss-area` output remain unchanged. Only this standalone binary invokes the
observer. It reads both RAM regions at the same paused frame boundary and must
not combine held-chord endpoint cartridge RAM with historical WRAM.

The [observation contract](../../../../benchmarks/search/depth-transfer/boss-observation-contract.md)
records source arguments, reference data, reset/gap/stale-byte counterexamples,
and the unresolved possibility of an invisible same-key slot reload. Its output
is observed HP decrease, not a proven lifetime damage total or search reward.

### Conditional encounter control diagnostic

An explicit optional request field, `counterfactual_resources`, accepts
`{"health":1999,"missiles":20}` only within the supplied state's earned
capacities. This standalone causal diagnostic changes the two BCD health bytes
and the missile-count byte at the paused boundary. It checks all RAM, the
physical clock and exact serialized LRAM/SRAM offsets, then updates only the
cached resource fields. A failed guard rolls back; no-op writes preserve the
full snapshot. The diagnostic parser is pinned to the existing Harmony/QuickNES
snapshot wrapper; another layout fails instead of guessing its offsets.

These are artificial interventions, not actions or states produced by search.
The probe exports before/after snapshots and `resource-operation.json` with the
prefix boundary, resource values and both snapshot hashes. Counterfactual
witnesses carry the same operation; held replay repeats the real prefix,
verifies and applies the operation, then executes the suffix. Such witnesses
must never be presented as ordinary prefix-plus-suffix inputs or fresh wins.
Absent this field, the original diagnostic and output representation remain
unchanged. The generic searcher and all search policies ignore this operation.

`metroid-control-probe draws REQUEST OUT` materializes frozen ordinary
`sample_chord` suffixes without emulation. `metroid-control-probe run CORE ROM
REQUEST OUT` requires their hash and the exact searched input, asset and positive
endpoint identities. It restores one unchanged encounter before each ordinary or
passive trial, checks the snapshot and raw context, then measures guarded HP
changes one frame at a time with a fresh baseline. Passive commands preserve the
paired hold durations. Early death can make actual arm costs different.

The request bounds seeds, commands, continuation frames and total physical work.
At most the first surviving damage and defeat tape per arm are retained; each
must replay twice from ordinary genesis with the original held commands and
matching emulator bytes. Per-trial flushed logs and final usage report actual
frames, including setup and replay. External wall, process-memory and output
limits are still required. This helper does not change search or supply fresh
validation. See the [contract](../../../../benchmarks/search/endpoint-encounter/control-contract.md).

### Archive challenge from a supplied searched state

`metroid-archive-challenge prepare REQUEST OUT` checks one bounded searched input
and its qualified mechanical/raw/emulator identity, replays it twice, and writes
the complete root snapshot and its digest. `run REQUEST OUT` requires that frozen
snapshot digest. It uses the existing generic `CampaignOrigin::SnapshotRoot`,
ordinary Metroid selector and action law, corrected terminal predicate, and a
registered Kraid/Ridley-defeat milestone. Archive actions and costs start at the
supplied root; the original prefix never enters search as a donor or solution.

The challenge bounds workers, jobs, frames, actions, memory, result slots, search
wall time and direct helper replay work. It preserves the stream and checkpoint,
requires full campaign replay, then verifies one milestone/champion input twice
from the root and twice as a complete prefix-plus-local input from genesis. All
four endpoints must match in snapshot and same-boundary raw context. An inherited
root event or an event beyond the admitted frame ceiling does not pass.

The command changes no engine, archive, target or legacy evaluator behavior.
Supplied-state results are diagnostic capability, never fresh discovery. Helper
setup/replay frames and campaign admitted/replayed-admitted frames are separate;
engine setup, unadmitted work and reconstruction remain additional unknown costs.
External process/output/wall caps and prospective registration are required.
See the [challenge contract](../../../../benchmarks/search/endpoint-encounter/archive-challenge-contract.md).

### Inspecting an existing checkpoint

`metroid-checkpoint-inspect inventory CHECKPOINT OUT` uses the current typed
checkpoint decoder to export cached IDs, mechanical states and retention cells
without initializing an emulator. It does not label cached history as active.
`inspect REQUEST OUT` pins the checkpoint, single-entry origin and assets,
checks the origin against its qualified snapshot/context, then restores and
reads each cached snapshot at the same paused boundary. Every restore/read must
preserve the entire snapshot and the physical frame counter. No actions or
search run; only one ordinary 929-frame constructor setup is permitted.

Inputs are limited to 32 MiB each and 1,000 unique checkpoint entries. Existing
outputs are refused. Register external process, wall and file limits before
native inspection. Raw HP is a state observation, not lifetime damage. Active
membership needs the producing run's exact replacement/retirement semantics
and evidence; it is not encoded in `SnapshotCheckpoint`.

`MetroidGame::with_retention_capture(path, identity)` provides an optional
exhaustive capture for separately qualified short replays. It uses the existing
read-only full-slot observer, requires the ordinary one-member local rule, and
records both snapshots and checkpoint-local inputs with the current incumbent's
stable ID. A missing incumbent snapshot remains unavailable. Exact-input
duplicates and empty-slot admissions are outside this observer; recorded replay
decisions must account for them separately. This capture does not decode HP,
restore an emulator, or feed observations back into search.

The framed postcard file binds the actual replay stream and origin hashes,
snapshot format and key policy. It permits at most 5,000 competitions, 4,096
actions per input, 256 KiB per encoded frame and 512 MiB per file. Encoding uses
one fixed 256 KiB buffer; temporary input reconstruction, snapshot clones and I/O
remain host overhead outside logical archive memory. Existing files are refused.
An error prevents a completion footer; the finish hook disables further capture.
`retention_capture::CaptureReader` streams one row at a time and rejects wrong
encodings, bounds violations, missing or inconsistent footers and trailing data.
Only a successful terminal read establishes file completeness. Native use still
requires pinned assets, exact per-job replay equality and external process,
wall, memory and output limits. The builder API adds no ordinary evaluator flag.

`metroid-retention-replay REQUEST OUT` is the standalone caller for the saved
short ordinary AP01 protocol. Its strict request pins the original stream,
single-snapshot origin, original final checkpoint, root snapshot, ROM, runtime
core and original core identity. `qualify` restores the known root and replays
the first four recorded jobs. `inspect` requires the successful qualification
report, bound to the same executable and inputs, before replaying the complete
stream with capture. Every recorded job must reproduce its frames, result
digest and ordered decisions; the full final raw checkpoint must also match.

Raw snapshots require the exact original core binary: QuickNES embeds that
binary's SHA-256 in each snapshot and rejects another build before importing
its payload. The caller rejects unequal original/runtime core identities before
constructing a target and preserves the original header and selected job bytes
verbatim. Reconstructing a state from its actions on another build needs a
separate protocol; changing a stream declaration cannot qualify raw import. A separate
paused target then reads captured states with exact restore and unchanged-clock
checks. Output includes raw contexts, qualified boss-slot observations, player
resources, local disposition and input/snapshot hashes. HP255 remains unavailable;
these are state comparisons, not lifetime damage or retention utility.

The program rejects incomplete lines, incompatible policies, unexpected skips
and bounds outside the supported protocol before replay. A request must cover
recorded work plus one worst-case divergent job's origin/suffix actions and both
constructor allowances; it cannot exceed 1.5M frames. A 1–300 second process
watchdog also bounds native work. External memory/process/output limits remain
required. Each input is at most 32 MiB; context output is at most 32 MiB in
addition to the capture limit. Failed replay work and the generic replayer's
unexposed constructor counter remain explicit accounting gaps, not zero work.
No native qualification or inspection is implied by successful source tests.

### Experimental local terminal retry

`MetroidGame::with_local_terminal_retry(true)` records
`local_terminal_retry=one_per_live_boundary_predrawn_attempts_v1`. The default
omits it and preserves legacy streams. Replay requires the identical policy.
Only independent `alphabet_only` draws and `admit_alive` are supported. The
shared `searcher::rollout::LocalRetry` helper uses the next already drawn command
after restoring the last live snapshot; failed attempts consume the original
cap and remain in observations, death counts, result digests and physical cost.
Emulator failures, victory and second consecutive death stop normally.

The standalone archive challenge accepts `local_terminal_retry: true`. If no
boss milestone occurs, it verifies the first surviving retry input against the
original worker-snapshot digest, then replays it twice locally and twice with
the searched prefix from genesis. A retry count without a surviving witness
cannot pass this qualification. This mechanism is unqualified for fresh-search
performance until its bounded native and matched-work gates pass.
