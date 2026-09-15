# Step 3: size continuation replay to the archive

Base: the step 2 merge commit. Line numbers from `58d07433`; refresh before
editing.

## What it is

Continuation replay propagates a better state at one slot to the slots
reached from it. When a slot's holder is replaced by a better state, every
recorded exit from that slot is replayed from the new holder. If the result
beats the holder at the destination, that slot's exits are queued in turn.
That chain is a wave, and it is the mechanism Antithesis credits with
finishing Metroid.

## What is wrong

`dissonance/searcher/src/search/continuation.rs` holds the exits in a bank
with fixed caps: 8,192 edges total, eight per slot, a queue of 1,024, oldest
edges evicted first, newest attempts popped first, one reservation in four,
and only when the run's mixture identifier names a continuation policy. The
caps exist so a fixed memory reserve can be charged before bootstrap
(`reserve_bytes`, 37-42). The Metroid controls hold 89,000 selection cells,
so the bank forgets most of the map, starting with the oldest edges, and a
wave dies within a few hops.

## What it becomes

| Piece | Today | After |
|---|---|---|
| edges | separate bank, 8,192 total, 8 per slot, FIFO eviction (`record`, 45-79) | one edge per ordered pair of depth-0 slots, holding the cheapest known tail between them; no count cap |
| memory | fixed reserve (`history_memory_bytes`, `archive.rs` 3083) | each edge charged when recorded, the way the input index is; released when `compact_history` (`archive.rs` 1375) drops a slot's last entry |
| trigger | strictly preferred replacement only (`archive.rs` 2195-2200) | any replacement at a slot, preferred or cheaper at equal preference (`insert_after` replaces on both, 2165-2168) |
| queue | capped `VecDeque`, popped newest first (`improved` 81, `pop` 95) | one pending entry per edge, updated to the latest parent, popped oldest first |
| attempts | the loop at `campaign.rs` 2488 runs until it finds a usable entry or empties the queue | at most 8 entries examined per reservation, then ordinary selection |
| tail length | `ACTION_CAP = 128` | the suffix shape's maximum |
| share | one reservation in four (`campaign.rs` 2487; replay check at 3569) | one in two while anything is pending |
| enablement | `enable_continuations(config.mixture.uses_continuations())` (`campaign.rs` 2367, 3366) | always on |
| identifiers | `energy_splice_continuation_v1`, `_v2`, `alphabet_continuation_v1` in `draw.rs` 121-165 | removed; recorded runs naming them fail to load with an error naming them; `ci.json` names `energy_splice_continuation_v1:6` and must change |
| replay | reconstructs a continuation job from its record (`campaign.rs` 3598) | runs the same dispatch loop against the bank it rebuilt, and checks the record matches |
| record | donor, leaf, tail | plus destination slot and wave length |
| accounting | dispatch totals | plus edges held, pending entries, continuation jobs run and their execution work, replays landing in their destination slot, replays that replaced the holder there, and the longest wave, per progress record |

The dispatch loop keeps its checks: skip a stale parent, bound the tail
cost, skip a fully archived prefix, record the tail as a splice record, pin
the origin. Results pass through ordinary retention. Isolated accounting
(replays do not bump barren counters or reward the splice strategy) is now
the only behaviour.

Bounds: pending entries never exceed the edge count; a reservation examines
at most 8 entries; at most every other reservation is a continuation.
Continuation jobs cost what their restore and tail cost, so the accounting
reports their execution work separately from the share of reservations.

## Hot paths

| Path | Runs | Allowed work per call |
|---|---|---|
| `insert_after` records an edge | every admission | one lookup and one insert on `(from, to)`, two set inserts |
| a replacement queues a slot's exits | every replacement | proportional to that slot's out-degree |
| a reservation pops | every other reservation | at most 8 entries examined |
| `compact_history` drops a slot | rarely | proportional to that slot's in-degree plus out-degree |
| the memory charge | every admission | incremental add |

Anything on the first three rows that walks the whole edge set or the whole
archive is a defect. Check with the throughput comparison in `README.md`.

## Steps

### 1. Data structures

Replace `ContinuationBank` with:

- `edges: BTreeMap<(K::Group, K::Group), Edge<A>>`, where `Edge` holds the
  tail, its cost, and the donor and leaf ids for the splice record.
- `exits: BTreeMap<K::Group, BTreeSet<K::Group>>`, out-edges per slot.
- `entrances: BTreeMap<K::Group, BTreeSet<K::Group>>`, in-edges per slot.
- `pending: VecDeque<(K::Group, K::Group)>` and
  `pending_parent: BTreeMap<(K::Group, K::Group), Pending>`, where
  `Pending` holds the parent id and the wave length.

`record(from, to, ...)` inserts, or replaces only when the new tail is
cheaper. `improved(place, parent, wave)` sets `pending_parent` for each
out-edge of `place` and pushes the pair onto `pending` only if it was not
already present. `pop()` takes from the front and skips a pair absent from
`pending_parent`. `remove_slot(slot)` deletes the slot's out-edges and
in-edges through both index sets and their `pending_parent` entries. When
`pending.len()` exceeds twice `pending_parent.len()`, rebuild `pending`
from the map in map order.

### 2. Memory

Charge each recorded edge's bytes at record time and release them in
`remove_slot`. Remove `reserve_bytes` and its use in `history_memory_bytes`.

### 3. Trigger, share and attempts

In `insert_after`, call `improved` on any replacement, with wave length 0
for an ordinary admission and the arriving job's wave plus one for a
continuation result. In the dispatch loop, change one in four to one in
two, examine at most 8 pending entries, and fall back to ordinary selection
when none is usable.

### 4. Replay

In the replay loop at `campaign.rs` 3567-3579, when the record's sequence
falls on a continuation reservation, run the same pop loop against the
replayed bank. If the loop yields a job, the record must be a continuation
job with the same parent, donor, leaf, tail, destination and wave;
otherwise the record must be an ordinary job. Either mismatch is an error
naming the sequence. The bank is rebuilt by replayed admissions and
replacements, so live and replay pop the same entries, discard the same
stale ones, and charge the same bytes.

### 5. Always on, identifiers removed, schema version

Make `enable_continuations` unconditional and delete the three
continuation mixture identifiers, their `DrawMixture` variants,
`isolates_continuations` and `uses_continuations`. Update
`benchmarks/search/ci.json` and any other manifest naming a removed
identifier. Set `CAMPAIGN_SCHEMA_VERSION` to 4, since the job record and
the reservation share changed.

### 6. Tail length

Tie the tail cap to the suffix shape's maximum and delete `ACTION_CAP`.

### 7. Tests

- Recording a cheaper tail for an existing pair replaces it; a costlier one
  does not.
- Two replacements at one slot in a row queue each exit once, with the
  latest parent and wave.
- The queue pops oldest first and skips removed pairs.
- Removing a slot drops its out-edges, its in-edges, their pending
  entries, and their memory charge; recording an edge into a recreated
  slot works afterwards.
- A reservation with 20 stale pending entries examines 8, runs an
  ordinary job, and the next continuation reservation continues from the
  ninth.
- Live and replay agree on edge count, pending count, memory charge and
  every admission, on a run with several outstanding reservations, stale
  attempts, and a memory budget tight enough to compact. The generic
  resource fixture in `campaign_continuation_tests.rs` is the base for
  this; extend it with many cheaper-at-equal-preference replacements at
  one slot and a slot that is compacted away and recreated.

### 8. Docs

Rewrite the continuation paragraphs in `dissonance/searcher/README.md`
(around 165-200): what a wave is, what is stored per edge, that it is always
on, the bounds, and the accounting fields.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel, long panel,
throughput.

Before the milestone numbers, read the accounting: how many edges, how many
replays landed in their recorded destination, how many replaced the holder
there, and how long waves ran. Then look at where health and missiles sit in
the Metroid census by area, compared with the step 2 run. If waves work,
resources at the frontier rise before any milestone moves.

If the landing share is low, so most replays miss their destination slot,
say so in the pull request and leave the mechanism in. The follow-up in that
case is replaying from the two or three nearest holders rather than one,
and it is a separate step, not a change to this one.
