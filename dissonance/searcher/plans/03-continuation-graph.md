# Step 5: continuation replay governed by feedback

Base: the step 4 merge commit (the strategy portfolio). The base still holds
the capped bank from main (`continuation.rs`: `EXIT_CAP`, `QUEUE_CAP`,
`ACTION_CAP`, `reserve_bytes`) and the three continuation mixture
identifiers; this step deletes them. The edge recording, `retain_slots`,
the replay placement at the reconstructed reservations,
`continuation_reservation_matches` and `ContinuationAccounting` on
`backup/searcher-groundwork-with-step3` are correct in shape and are the
starting point for the parts of the same name below. The pending queue,
the trigger, the share and the feedback are new. Line numbers name the
branch they are from.

## What it is

Continuation replay propagates a better state at one slot to the slots
reached from it. The archive records one edge per ordered pair of depth-0
slots, holding the cheapest known tail between them. When a slot's holder
is replaced by a strictly preferred state, the slot is queued, and a
reservation that takes the queue replays that slot's exits from the new
holder, a few at a time. A result that lands at the recorded destination
and is strictly preferred there queues that slot in turn. That chain is a
wave.

## What the first build measured

Built with a trigger on any replacement and a fixed share of one
reservation in two or four, it lost to continuations-off on every SMB and
Metroid cell. The run reports say why.

| Run | Replays | Landed at the recorded destination | Replaced the holder there | Pending at the end |
|---|---|---|---|---|
| Metroid long panel, 1 in 4, seeds 3, 4, 5 | 750K each | 73K, 73K, 70K | 69K, 69K, 66K | 475K of 631K edges |
| SMB regression, 1 in 2, one cell | 822K | 9.8K | 7.7K | 233K of 847K edges |

A recorded tail replayed from a different holder of the same coarse slot
diverges nine times in ten on Metroid and ninety-nine in a hundred on SMB.
Antithesis's connectivity graph joins exact system states, so a replayed
transition starts from the state it was recorded from; a Dissonance slot is
a 32-pixel cell with posture and door state, and its new holder is a
different exact state. Every replay still costs one execution, taken from
exploration one for one. SMB has no `preference_cmp`, so every cheaper
arrival re-queued a slot's exits and the queue never drained. The share was
fixed, and continuation was kept apart from the barren-energy feedback the
other draw strategies have, so it could not turn itself down.

## What it becomes

| Piece | First build | After |
|---|---|---|
| when the bank exists | always | only for a workload that declares a preference (after step 4, a non-empty preference list; the trait default declares none). Without one no edge is recorded, nothing is charged, and the stream is unchanged |
| trigger | any replacement, including cheaper at equal preference | a strictly preferred replacement only: `preference_cmp` returned `Greater` for the candidate against the displaced holder, under the preference the replacement was chosen for. The slot is queued once whichever champion improved |
| pending queue | one entry per edge, pushed eagerly for every exit of the improved slot (`continuation.rs` 119-132 on the backup branch) | one entry per slot: `pending: BTreeMap<u64, Group>` keyed by an insertion sequence, and `pending_slot: BTreeMap<Group, Pending { sequence, parent, wave, cursor: Option<Group> }>`. Queuing a slot is two map inserts, or an update of parent, wave and cursor if it is already pending. Popping takes the front slot's next exit after `cursor` from its `exits` set, advances the cursor, and moves the slot to the back under a fresh sequence, so slots rotate and a slot improved on every reservation cannot hold the front; a slot whose exits are exhausted is removed instead. `remove_slot` deletes both map entries by sequence, and when removing a slot empties a source slot's `exits` set, that source's pending entry goes too. No node is ever left behind |
| share | one reservation in `CONTINUATION_RESERVATION_STRIDE` (`campaign.rs` 41, 91 on the backup branch) whenever anything is pending | a reservation attempts a continuation with probability `e / (e + 256)`, where `e = energy_share(continuation_barren, CONTINUATION_ENERGY_SCALE)` (`energy_share` at `draw.rs` 183 on `origin/searcher-groundwork`) and the scale is one constant, 6. Fresh is one in two, fully barren is one in 257 |
| the draw | none | when the queue is non-empty, `RomuDuoJrRand::with_seed(campaign_seed ^ reservation_index).below(e + 256) < e`; no worker random state, so replay recomputes it at the reconstructed reservation |
| feedback | replays bump no counter and reward no strategy | `continuation_barren` is its own field beside `MixtureEnergy`, never in the array that `splice_weights` and `biased_weight` normalise (`draw.rs` 190-205), so the table, splice and alphabet weights are unchanged. A continuation job's admission resets it when the job opened a new slot and increments it otherwise; a landed or replaced result is reported and not rewarded, since a wave would otherwise reward itself |
| edges and memory | one edge per slot pair, charged per edge, `retain_slots` on compaction | unchanged for edges; each pending entry is charged its two map nodes when queued and released when removed |
| replay | pops at the reconstructed reservation (prefill and replenish), checks the record | unchanged, plus the recorded `continuation_energy` must equal the rebuilt one |
| record | destination, wave, tail | plus `continuation_energy: u16` on every job at a reservation where the queue was non-empty |
| accounting | edges, pending, jobs, execution work, landed, replaced, longest wave | plus `continuation_barren`, `e`, reservations that drew, reservations that took a continuation, and jobs that opened a new slot |

Work per path: recording an edge at admission is one lookup and one insert,
queuing a slot at a replacement is two map operations, a reservation takes
at most 8 exits, and `remove_slot` is proportional to that slot's degree.
Nothing enumerates a slot's exits at admission or replacement time.

Bounds: pending slots never exceed the slots with edges; a reservation takes
at most 8 exits; the attempt probability is at most one half and, after
`8 * CONTINUATION_ENERGY_SCALE` barren jobs in a row, one in 257. One job
that opens a new slot restores it. Exact seeded draws are what a test
checks, never a share over a run.

The dispatch loop keeps its checks: skip a stale parent, bound the tail to
the suffix shape's maximum, skip a fully archived prefix, record the tail as
a splice record, pin the origin. Results pass through ordinary retention.

## Steps

### 1. Delete the old bank and identifiers

Remove the capped `ContinuationBank`, `enable_continuations`, the three
continuation mixture identifiers, `isolates_continuations` and
`uses_continuations`. `benchmarks/search/ci.json` names
`energy_splice_continuation_v1:6` and moves to `energy_splice:6`.

### 2. Bank

Write `continuation.rs` with the edge maps, `exits`, `entrances`,
`retain_slots` and `remove_slot` from the backup branch, and the per-slot
pending queue from the table. `improved(slot, parent, wave)` queues or
updates; `pop()` returns the next exit of the front slot as a
`Continuation` or `None`. The bank is constructed only when the workload
declares a preference; otherwise the archive holds no bank and
`insert_after` records nothing.

### 3. Trigger

In `insert_after`, call `improved` only when the replacement was chosen on
`Ordering::Greater` under some preference, never on the equal-preference
cheaper path. Step 4 records which preference a replacement was chosen
under; read that.

### 4. Share and feedback

Add `continuation_barren: u64` to the coordinator state beside
`mixture_energy`, `continuation_energy() -> u16` computing `e`, and the
seeded draw at each reservation with a non-empty queue. On a hit, take up
to 8 exits through the dispatch loop. A continuation job's admission
updates the counter from whether it opened a new slot, in
`record_mixture_outcome` (`campaign.rs` 855 on `origin/searcher-groundwork`)
or beside it; the three draw strategies' outcomes are untouched.

### 5. Record and replay

Add `continuation_energy` to the job record at reservations where the queue
was non-empty. Replay rebuilds the counter from replayed admissions,
recomputes `e` and the draw at each reconstructed reservation, and errors on
a mismatch naming the sequence. Set `CAMPAIGN_SCHEMA_VERSION` up by one.

### 6. Accounting

Extend `ContinuationAccounting` with the fields in the table and report it
on every progress record.

### 7. Tests

- Bank: a cheaper tail replaces a pair's edge and a costlier one does not;
  queuing a slot twice updates its parent and resets its cursor and leaves
  one entry at its old position; popping walks a slot's exits in set order,
  moves the slot to the back after each exit, and removes it when
  exhausted; two pending slots A and B where A has many exits and is
  improved between every pop still alternate, so B's exits run; deleting
  and recreating one slot many times leaves the queue's physical size equal
  to its reported size and keeps the order of unrelated slots;
  `remove_slot` releases the edge charge, the slot's pending entry, and the
  pending entry of a source slot left with no exits.
- Trigger: a cheaper arrival at equal preference queues nothing; a strictly
  preferred arrival queues the slot once, also when it improves two
  champions.
- No preference: a key with the default `preference_cmp` runs a fixture
  campaign under memory pressure with no bank, zero edges, zero continuation
  charge, and a stream equal to the base commit's apart from the schema
  version and the new record field.
- Feedback: after `8 * CONTINUATION_ENERGY_SCALE` barren jobs the draw
  is at the floor; one job that opens a new slot restores it; landed and
  replaced results without a new slot increment the counter; the table,
  splice and alphabet weights are the same before and after any
  continuation outcome.
- A slot with 500 exits improved 100 times performs no work proportional
  to 500 at replacement time, measured by a counter on the bank.
- Replay: live and replay agree on the counter, `e`, every draw, edge count,
  pending count and memory charge, with several reservations in flight,
  stale attempts, and a memory budget tight enough to compact. Base this on
  the fixture in `campaign_continuation_tests.rs` on the backup branch and
  replace its share-over-the-run assertion with exact seeded draw checks.
- A replay whose recorded `continuation_energy` disagrees with the rebuilt
  value fails naming the sequence.

### 8. Docs

Rewrite the continuation paragraphs in `dissonance/searcher/README.md`:
what a wave is, what an edge holds, the trigger, the share rule and its
floor, when the bank exists, and the accounting fields.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel, long panel with
both manifests, throughput, memory.

The SMB regression must match the step 4 run cell for cell, since SMB
declares no preference and holds no bank. If it differs, find why before
the panels.

Read the continuation accounting before any milestone number: the share of
reservations taken over the run, how many jobs landed, how many replaced,
how many opened a new slot, and the longest wave. Then compare occupied
cells, items and tanks per seed with the step 4 run, and watch the film for
the seeds that differ. Read throughput and memory against step 4 as well;
edge recording and the bank's charge are paid whatever the share.

This step is an experiment. Apply the README's regression rule against the
step 4 run, comparing at matched executions and again at matched execution
work, since continuation jobs cost more or less than ordinary ones: every
seed behind on a milestone step 4 reached on every seed, or every seed
behind on occupied cells, is a rejection, whatever the share was. A share
that sat at the floor with the panels level is the feedback working and
the mechanism stays. A share that stayed high with the panels level is a
result to write down.
