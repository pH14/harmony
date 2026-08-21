# Sol World 8-4 p153 regression-bridge H8 harvest

Status: preregistered after the registered p153 normal-endpoint-harvest result
commit and before recipe materialization, ROM loading, implementation sealing,
or live emulation.

## Question and authority

The exact p153 source has stopped under 6,144 ordinary one-action draws. Every
one of the 4,572 draws whose parent was the source itself ended live at
progress 89 through 107, and no lineage up to depth 7 exceeded progress 114.
Progress is `screen_page*16 + screen_x/16`, so the drop from 153 to about 89
is exactly four pages, the same shape as the registered p73 regression, which
the p73 regression-bridge harvest crossed at depths 6 through 8. This harvest
asks the same narrow existence question at the new source: does any fixed
eight-action opaque continuation bridge that temporary regression and yield a
normal live endpoint beyond p153?

The p73 one-action duration closure and paired midpoint compaction canary are
not repeated here. Their p73 answers were negative, and the p153 ordinary
harvest already recorded that every one-action successor of the source
regresses; the cheapest decisive next test is the mechanism that crossed p73.

This is an outcome-rate-uncontrolled harvest, not a policy comparison. A
positive result proves only that one registered short sequence from the fixed
p153 state is adoptable. The structural classification separately records
whether multiple distinct first-step regressions bridge the source watermark.
No fixed-H8 search policy is promoted by this run.

## Frozen source and provenance

- Code base before this experiment and registered p153 harvest result:
  `7312116a5280a7937b18e31c09497d78a18cc955`.
- p153 harvest preregistration `3c264bf1aecc49cb6f04db70d41e05f9fac4b9fd`,
  implementation `d6690276acddd7d48a6f29ee8e1d67778fb8c288`, report SHA-256
  `c4499e7a8af1e2c2683b0fb40c0923e9ace320fb930fa5597f3bd892128cd26f`.
- p73 regression-bridge precedent: preregistration
  `ca7a7b2239a6fa6b44e1e0cb87d75a405b3c109b`, implementation
  `a8ba2346cc99c8ae78a8a419d2574c97c87dfe32`, report SHA-256
  `b33441042225e4a047178f708acc7b97e396e003b6212c065c21b314ed979abd`.
- Exact source path for launch only:
  `/root/harmony-smb-sol-w8-4-p113-harvest-c765fcf4/results/adopted-world-8-4-progress-153-input.json`.
- Compact and semantic input SHA-256:
  `14af93bd006ba77cea923ab31cb7aa8ac0ad903a7bc65d5a378c92ccc337300b`;
  114,838 bytes, 3,576 actions, 168,594 replay frames.
- Exact endpoint and maximum watermark `(7,3,153)`; mechanical endpoint
  `{player_y_bucket:11, player_engine_state:8, dead:false, flag_active:false}`;
  Frozen key `{world:7,level:3,progress:153,player_y_bucket:11,
  player_engine_state:8,state_fingerprint:9,room_x_bucket:0}`.
- WRAM SHA-256
  `897c7bc0df63a68249b75e81a8bfc8ea3a87a7c872241d4e51a2819ff39689c5`;
  snapshot SHA-256
  `329594d247d5a97ea59a0e7ec1b0856cfb0388141941f05062e4d6641adf5344`;
  final chord `(0x82,104)` and the already frozen milestones
  `{max_1_1_scroll_bucket:195,reached_1_1_flag:true,reached_1_2:true,
  reached_onward:true}`.
- ROM SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

The binary may read only the compact source, ROM, and its current executable.
It must not read the p153 harvest report, its candidate snapshots, prior
recipes, or any other campaign/canary artifact. All cited result facts are
header provenance only.

## Frozen recipes

Seed label `sol-restart-w8-4-p153-regression-bridge-h8-harvest-v1` has SHA-256
`ca0f43caeb533cea6732df8a41df6dd0999aacba9124d453b2a3c484204c52da`;
its first eight digest bytes interpreted little-endian are master seed
`16878457775653588938`.

There are exactly 1,680 streams `t=0..1679`, each with eight opaque actions.
The first action exhaustively repeats the sealed grid: `mask =
SOURCE_MASKS[t/120]`, `duration = (t%120)+1`, with masks in exact ascending
order `[0,1,2,16,32,64,66,128,129,130,131,192,193,194]`. This is the exact
set of distinct opaque masks present in the 3,576-action source; the binary
must verify that set before materializing recipes.

For action ordinal `j=1..7`, compute
`digest = SHA256(master_seed_u64_le || ASCII("regression-bridge-source-index")
|| t_u64_le || j_u64_le)`, interpret its first eight bytes little-endian, take
modulo 3,576, and copy the complete source `ButtonChord` at that occurrence.
There is no retry, rejection, filtering, semantic decoding, outcome feedback,
or learned table. Modulo reduction is accepted and recorded.

After validating the compact input but before constructing a target or loading
the ROM, materialize all recipes in stream/action order. The exact recipe
identity is `serde_json::to_vec` of one bare ordered Vec whose elements are
`(t_u64, first_mask_u8, first_duration_u8, actions_Vec<ButtonChord>,
tail_source_indices_Vec<u64>)`. Require exactly 1,680 elements, eight actions
and seven indices per element. For each stream separately serialize the exact
bare tuple `(first_mask_u8,first_duration_u8,actions_Vec<ButtonChord>,
tail_source_indices_Vec<u64>)`, excluding `t` and every wrapper; require all
1,680 projection byte vectors pairwise distinct. Record the global bytes/SHA and
every projection bytes/SHA. Collision or identity mismatch is integrity STOP;
never retry.

## Baseline and execution

Replay the source once from genesis and require exact source byte/semantic,
action-count/frame, endpoint/maximum, WRAM, snapshot, key, milestones, and final
chord evidence. From the endpoint run the normal 45-frame mask-0 source probe,
require `ExitKind::Ok` and survival, then restore and byte-verify the exact
source snapshot. Baseline setup, replay, and probe work are separate.

Use exactly 12 persistent targets. Assign stream `t` to worker `t%12`, process
ascending stream per worker, buffer all replies, and consume/report strict
stream order. Completion timing reaches no bytes. Every worker initialization
or stream failure yields a reply for every assigned ordinal; missing,
duplicate, wrong-worker, out-of-range, or non-Ok evidence is deterministic
integrity STOP selected in canonical stream order.

For each stream independently, restore and verify the exact p153 snapshot, then
execute its eight actions sequentially without intermediate reset. At every
completed action boundary record stream/depth, opaque action and registered
source index when present, exact cumulative input hash/action count, requested
and actual work, observation/mechanical/watermark/milestones, transient maximum,
raw WRAM SHA, trace SHA, death, and failure. Snapshot every live boundary and
record snapshot SHA and Frozen key. Ok-death terminates only that stream; later
depths are explicitly absent. A later death never erases earlier evidence.

Only a live endpoint strictly beyond `(7,3,153)` receives the unchanged normal
viability probe: ordered masks `[00,01,81]`, at most 45 frames each, restore and
verify before every attempt, short-circuit on first survivor, and exact restore
afterward. Probe non-Ok or restore mismatch is integrity STOP. Non-strict live
boundaries are not probed and cannot be adopted.

No archive, selector, insertion, replacement, parent feedback, waypoint,
snapback, compaction, prefix shortening, or cross-stream state exists. The only
state carry is the registered within-stream sequence.

## Bounds and decisions

At most 13,440 action boundaries execute. First-action requested work is at
most `14*sum(1..120)=101,640`; the seven tail actions add at most
`1,680*7*120=1,411,200`, so action work is at most 1,512,840 frames. Conditional
strict-candidate probes are at most `13,440*3*45=1,814,400`. Source replay is
168,594, source probe 45, and one baseline plus 12 worker setups is 4,693. The
checked hard total is **3,500,572 frames**. Maximum input length is 3,584, below
the 4,096 action limit. Wall time has no authority.

An eligible boundary is newly executed, `ExitKind::Ok`, alive, strictly beyond
the full source watermark, and probe-surviving. Deduplicate eligible evidence by
exact semantic input SHA using lowest `(stream,depth)` ownership computed from
all materialized inputs before execution; only afterward apply observed
eligibility. Rank eligible candidates by full watermark descending, action
count ascending, semantic input SHA ascending, then stream/depth ascending.

The adoption verdict is **ADOPT** iff at least one eligible boundary exists;
embed the sole champion exact input plus complete boundary/probe/lineage/work
evidence. Otherwise it is **NO_ADOPT**.

A regression bridge additionally requires depth `2..=8`, a completed live
depth-1 endpoint strictly below `(7,3,153)`, and a later eligible boundary from
the same stream. Classify **MULTIPLE_REGRESSION_BRIDGES** iff at least two
canonical bridges from at least two distinct first-action chords have at least
two distinct eligible input hashes and snapshot hashes; **SINGLE_REGRESSION_BRIDGE**
iff at least one bridge exists but the multiple rule fails; otherwise
**NO_REGRESSION_BRIDGE**. This classification is diagnostic and independent of
adoption. It authorizes no H8 policy.

World 8-4 is the final level. Terminal-like evidence (flag, credits-like
screen, or a watermark outside the registered ordering) is diagnostic only.
Completion is never declared from this run; it requires a separately frozen
mechanical completion predicate and artifact-only confirmation.

`NO_ADOPT` closes this fixed H8 source-marginal continuation without rerun,
enlargement, threshold relaxation, or post-hoc recipe change and routes the next
work to a separately preregistered novel-mask or longer structured search. Emit
create-new canonical NDJSON with header, baseline, recipes, ordered boundary
records, classifications, summary, and source/ROM/executable/bin/module/config/
recipe/trace/body/whole-file hashes; paths, timestamps, and completion order
must not enter canonical bytes.
