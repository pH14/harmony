# Sol World 8-4 p73 regression-bridge H8 harvest

Status: preregistered after the registered duration-closure result commit and
before recipe materialization, ROM loading, implementation sealing, or live
emulation.

## Question and authority

The exact p73 source has now stopped under 6,144 ordinary one-action draws,
paired midpoint compaction, and the exhaustive 14 observed masks crossed with
every legal hold duration. The exhaustive census produced 1,652 live first-step
snapshots but no endpoint or transient beyond `(7,3,73)`; those endpoints
collapsed to 30 decoded states at progress 9 through 18. This harvest asks the
narrow existence question: does any fixed eight-action opaque continuation
bridge that temporary regression and yield a normal live endpoint beyond p73?

This is an outcome-rate-uncontrolled harvest, not a policy comparison. A
positive result proves only that one registered short sequence from the fixed
p73 state is adoptable. The structural classification separately records
whether multiple distinct first-step regressions bridge the source watermark.
No fixed-H8 search policy is promoted by this run.

## Frozen source and provenance

- Code base before this experiment and registered duration-closure result:
  `55bdab5f965a300f6d12529bc322ebab421f63d6`.
- Duration-closure preregistration `6078a7c781de16b0bc75c152481cf158a5669ee3`,
  implementation `00fd0a1ae25e08afc6302882c168084f3ae29eac`,
  report SHA-256
  `e4d9d86738c546048d67dda3adea15032bda5dbb65e3afcc212f958977a7999a`,
  and registered-result document SHA-256
  `b94a8dde656d30ba2482975b4a68c19664a48deb42aaf9593c840831edc8e06f`.
- Exact source path for launch only:
  `/root/harmony-smb-sol-w8-4-p61-harvest-v3-3aaeb783/results/adopted-world-8-4-progress-73-input.json`.
- Compact and semantic input SHA-256:
  `d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c`;
  114,128 bytes, 3,554 actions, 167,340 replay frames.
- Exact endpoint and maximum watermark `(7,3,73)`; mechanical endpoint
  `{player_y_bucket:8, player_engine_state:8, dead:false, flag_active:false}`;
  Frozen key `{world:7,level:3,progress:73,player_y_bucket:8,
  player_engine_state:8,state_fingerprint:60,room_x_bucket:0}`.
- WRAM SHA-256
  `bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734`;
  snapshot SHA-256
  `3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927`;
  final chord `(0x00,3)` and the already frozen milestones.
- ROM SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

The binary may read only the compact source, ROM, and its current executable.
It must not read the duration-closure report, its candidate snapshots, prior
recipes, or any other campaign/canary artifact. All cited result facts are
header provenance only.

## Frozen recipes

Seed label `sol-restart-w8-4-p73-regression-bridge-h8-harvest-v1` has SHA-256
`767314ca73cc0475e28f16e6560d5bfd37d2ae4b2534e5ca7f616aeb97d7f1a3`;
its first eight digest bytes interpreted little-endian are master seed
`8432089200028054390`.

There are exactly 1,680 streams `t=0..1679`, each with eight opaque actions.
The first action exhaustively repeats the sealed grid: `mask =
SOURCE_MASKS[t/120]`, `duration = (t%120)+1`, with masks in exact ascending
order `[0,1,2,16,32,64,66,128,129,130,131,192,193,194]`.

For action ordinal `j=1..7`, compute
`digest = SHA256(master_seed_u64_le || ASCII("regression-bridge-source-index")
|| t_u64_le || j_u64_le)`, interpret its first eight bytes little-endian, take
modulo 3,554, and copy the complete source `ButtonChord` at that occurrence.
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

For each stream independently, restore and verify the exact p73 snapshot, then
execute its eight actions sequentially without intermediate reset. At every
completed action boundary record stream/depth, opaque action and registered
source index when present, exact cumulative input hash/action count, requested
and actual work, observation/mechanical/watermark/milestones, transient maximum,
raw WRAM SHA, trace SHA, death, and failure. Snapshot every live boundary and
record snapshot SHA and Frozen key. Ok-death terminates only that stream; later
depths are explicitly absent. A later death never erases earlier evidence.

Only a live endpoint strictly beyond `(7,3,73)` receives the unchanged normal
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
167,340, source probe 45, and one baseline plus 12 worker setups is 4,693. The
checked hard total is **3,499,318 frames**. Maximum input length is 3,562, below
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
depth-1 endpoint strictly below `(7,3,73)`, and a later eligible boundary from
the same stream. Classify **MULTIPLE_REGRESSION_BRIDGES** iff at least two
canonical bridges from at least two distinct first-action chords have at least
two distinct eligible input hashes and snapshot hashes; **SINGLE_REGRESSION_BRIDGE**
iff at least one bridge exists but the multiple rule fails; otherwise
**NO_REGRESSION_BRIDGE**. This classification is diagnostic and independent of
adoption. It authorizes no H8 policy; multiple evidence may authorize only a
separately preregistered regression-tolerant parent-selection canary.

`NO_ADOPT` closes this fixed H8 source-marginal continuation without rerun,
enlargement, threshold relaxation, or post-hoc recipe change and routes the next
work to a separately preregistered novel-mask or longer structured search. Emit
create-new canonical NDJSON with header, baseline, recipes, ordered boundary
records, classifications, summary, and source/ROM/executable/bin/module/config/
recipe/trace/body/whole-file hashes; paths, timestamps, and completion order
must not enter canonical bytes.

## Registered result

Implementation commit `a8ba2346cc99c8ae78a8a419d2574c97c87dfe32`
used module SHA-256
`71605f0497a34284fbd0d48da25e937d904ca7f3985e645bc5f531030abff4bc`,
bin-source SHA-256
`08aa386de3ec3b0edc4e3faaabd4f7052600f2981c58421b15ca04c2aa8a4480`,
and release-executable SHA-256
`0d4cd2bf99c3a268773eeca5b2b0e9931082958555339e07e0a0ccc800803cf8`.
The sealed recipe was 515,034 bytes with registered SHA-256
`d457c4b075f2452439681ddfd5629802e0d16100be920819455e77e097c58d54`.
The sole run completed successfully and produced 14,074 NDJSON lines,
89,748,251 bytes, whole-file SHA-256
`b33441042225e4a047178f708acc7b97e396e003b6212c065c21b314ed979abd`,
and body SHA-256
`ed489654b5c6b1301711fb0d71cd402eaed189b678905aa19982e8d789aa052d`.
Standard error was empty and standard output bound the same report hash.

The registered verdicts are **MULTIPLE_REGRESSION_BRIDGES** and **ADOPT**.
There are eight canonical eligible boundary inputs with eight distinct snapshot
hashes, arising from three distinct first actions. Streams 1371, 1396, and 1416
began with opaque chords `(0xc0,52)`, `(0xc0,77)`, and `(0xc0,97)` respectively;
all three first boundaries were live at `(7,3,9)` with distinct Frozen state
fingerprints. Later depths 6 through 8 crossed the source watermark. This is
direct evidence that a temporary target-reported progress regression can carry
state needed by a later ordinary action boundary; no single action in the prior
exhaustive census could do so.

The deterministic champion is stream 1416 depth 8 at watermark `(7,3,113)`.
Its exact eight-action suffix is `[(0xc0,97),(0x83,8),(0x80,5),(0x02,7),
(0x82,12),(0x20,112),(0x82,10),(0x80,114)]`. The 3,562-action semantic input
SHA-256 is
`0b72eafdf81670fdf40ef80dab9226ddbee7c855728661f893816789fb24239f`;
the compact 114,388-byte adopted file has the same SHA. Endpoint WRAM SHA-256 is
`3bcdfbb5291fdfbf94ed016a77783e6bbb4b400c3ae24dc8d73f5d3ea844a24c`,
snapshot SHA-256 is
`0e87a78fc87df608fb466cd94154e814e095dad9eb2956edfaebba7b34080f00`,
and Frozen key is `{world:7,level:3,progress:113,player_y_bucket:11,
player_engine_state:8,state_fingerprint:59}`. It survived the first normal
mask-0 45-frame probe. The authorized artifact is
`/root/harmony-smb-sol-w8-4-p73-regression-bridge-a8ba2346/results/adopted-world-8-4-progress-113-input.json`.

Checked work was 4,693 setup + 167,340 source replay + 45 source probe +
597,845 action + 360 strict-candidate probe = **770,283 frames**, below the
3,499,318-frame cap. The exact p113 champion is authorized as the next source
only after a fresh genesis replay reproduces all registered evidence. This
result does not promote H8 as a general policy. It does authorize a separately
preregistered regression-tolerant parent-selection canary if later structural
work is needed.
