# Step 5: continuation replay governed by feedback

Base: the step 4 merge commit (the strategy portfolio). The bank, the
replay placement and the accounting on `backup/searcher-groundwork-with-step3`
are correct and are the starting shapes for this step; the trigger, the
share and the outcome feedback are rewritten here. Line numbers below name
that branch where they name code that is not on the base.

## What it is

Continuation replay propagates a better state at one slot to the slots
reached from it. The archive records one edge per ordered pair of depth-0
slots, holding the cheapest known tail between them. When a slot's holder is
replaced by a strictly preferred state, each of that slot's exits is queued,
and a queued exit is replayed from the new holder. A result that lands at
the recorded destination and is strictly preferred there queues that slot's
exits in turn. That chain is a wave.

## What the first build measured

Built with a trigger on any replacement and a fixed share of one reservation
in two or four, it lost to continuations-off on every SMB and Metroid cell.
The run reports say why.

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
| trigger | any replacement, including cheaper at equal preference | a strictly preferred replacement only: `preference_cmp` returned `Greater` for the candidate against the displaced holder. Under the portfolio, a champion strictly improved under its own preference; one pending entry per edge whichever champion improved |
| share | one reservation in `CONTINUATION_RESERVATION_STRIDE` (`campaign.rs` 41, 91) whenever anything is pending | a reservation attempts a continuation with probability `e / (e + 256)`, where `e` is `energy_share(barren, scale)` for a fourth entry in `MixtureEnergy` (`draw.rs` 179) and `scale` is one constant, `CONTINUATION_ENERGY_SCALE = 6`, the value the panels use for the draw strategies; fresh is one in two, fully barren is one in 257 |
| the draw | none | when the queue is non-empty, `RomuDuoJrRand::with_seed(campaign_seed ^ reservation_index).below(e + 256) < e`; no worker random state, so replay recomputes it at the reconstructed reservation |
| feedback | replays bump no counter and reward no strategy | `record_outcome(EnergyStrategy::Continuation, new_slot)` on the job's admission, as the three draw strategies do at `campaign.rs` 857-880; a landed or replaced result is reported and not rewarded, since a wave would otherwise reward itself |
| queue | one pending entry per edge, oldest first, tombstones counted, at most 8 taken per reservation | unchanged |
| edges and memory | one edge per slot pair, charged per edge, `retain_slots` on compaction | unchanged |
| replay | pops at the reconstructed reservation (prefill and replenish), checks the record | unchanged, plus the recorded `continuation_energy` must equal the rebuilt one |
| record | destination, wave, tail | plus `continuation_energy: u16` on every job at a reservation where the queue was non-empty |
| accounting | edges, pending, jobs, execution work, landed, replaced, longest wave | plus the barren counter, `e`, reservations that drew for a continuation, reservations that took one, and jobs that opened a new slot |
| enablement | always on | always on; a workload with no preference never triggers, so SMB pays nothing |

Bounds: pending entries never exceed the edge count; a reservation takes at
most 8 queue entries; at most one reservation in two is a continuation, and
after `8 * scale` barren jobs in a row, one in 257. One productive job
returns the share to one in two.

The dispatch loop keeps its checks: skip a stale parent, bound the tail to
the suffix shape's maximum, skip a fully archived prefix, record the tail as
a splice record, pin the origin. Results pass through ordinary retention.

## Steps

### 1. Bank and replay

Take `continuation.rs`, the `insert_after` edge recording, `retain_slots`
in compaction, the pop loop at the prefill and replenish reservations, and
`continuation_reservation_matches` from the backup branch. Delete
`CONTINUATION_RESERVATION_STRIDE` and `continuation_reservation`.

### 2. Trigger

In `insert_after`, call `improved` only when the replacement was chosen on
`Ordering::Greater`, never on the equal-preference cheaper path. Under the
portfolio, that is the comparison under the preference whose champion is
displaced; coalesce so an entry improving two champions queues each exit
once.

### 3. Share and feedback

Add `Continuation` to `EnergyStrategy` and a fourth barren counter to
`MixtureEnergy`, with `continuation_energy(scale) -> u16` returning `e`.
At each reservation with a non-empty queue, make the seeded draw above; on
a hit, take up to 8 entries. A job's admission calls `record_outcome` with
the continuation strategy and whether it opened a new slot. The scale is the
one constant under every mixture, since `alphabet_only` carries none.

### 4. Record and replay

Add `continuation_energy` to the job record at reservations where the queue
was non-empty. Replay rebuilds the counter from replayed admissions, recomputes
`e` and the draw at each reconstructed reservation, and errors on a mismatch
naming the sequence. Set `CAMPAIGN_SCHEMA_VERSION` up by one.

### 5. Accounting

Extend `ContinuationAccounting` with the fields in the table. Report it on
every progress record as the backup branch does.

### 6. Tests

- The bank tests from the backup branch pass unchanged.
- A cheaper arrival at equal preference queues nothing; a strictly preferred
  arrival queues each exit once.
- A key with no `preference_cmp` runs a whole fixture campaign with zero
  continuation jobs and a stream identical to the same campaign on the base
  commit apart from the schema version and the new record field.
- After `8 * scale` barren continuation jobs the share is at the floor; one
  job that opens a new slot restores it.
- Live and replay agree on the counter, `e`, every draw, edge count, pending
  count and memory charge, with several reservations in flight, stale
  attempts, and a memory budget tight enough to compact. Base this on the
  fixture in `campaign_continuation_tests.rs`.
- A replay whose recorded `continuation_energy` disagrees with the rebuilt
  value fails naming the sequence.

### 7. Docs

Rewrite the continuation paragraphs in `dissonance/searcher/README.md`:
what a wave is, what an edge holds, the trigger, the share rule and its
floor, and the accounting fields.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel, long panel with
both manifests, throughput.

Read the continuation accounting before any milestone number: the share of
reservations taken over the run, how many jobs landed, how many replaced,
how many opened a new slot, and the longest wave. If the share sits at the
floor for most of the run, the feedback is doing its job and the mechanism
costs almost nothing; that is a valid outcome and the mechanism stays in.
Then compare occupied cells, items and tanks per seed with the step 4 run,
and watch the film for the seeds that differ.

The SMB regression must match the step 4 run cell for cell, since SMB never
triggers. If it differs, find why before the panels.

This step is an experiment. It is rejected only if a panel is worse with the
share at the floor, which would mean the bookkeeping itself is on a hot
path; a share that stays high and a panel that is no better is a result to
write down, not a reason to drop it.
