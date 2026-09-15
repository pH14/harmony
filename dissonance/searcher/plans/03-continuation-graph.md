# Step 3: size continuation replay to the archive

Base: the step 2 merge commit. Line numbers from `58d07433`; refresh before
editing.

## What it is

Continuation replay propagates a better state at one slot to the slots
reached from it. When a slot's holder is replaced by a better state, every
recorded exit from that slot is replayed from the new holder. If the result
beats the holder at the destination, that slot's exits are queued in turn.
That chain is a wave, and it is the mechanism Antithesis credits with
finishing Metroid: a better resource state at one place propagates
downstream instead of every downstream place having to be rediscovered.

## What is wrong

`dissonance/searcher/src/search/continuation.rs` holds the exits in a bank
with fixed caps: 8,192 edges total, eight per slot, a queue of 1,024, oldest
edges evicted first, newest attempts popped first, one reservation in four,
and only when the run's mixture identifier names a continuation policy. The
caps exist so a fixed memory reserve can be charged before bootstrap
(`reserve_bytes`, 37-42). The Metroid controls hold 89,000 selection cells,
so the bank forgets most of the map, starting with the oldest edges, and a
wave dies within a few hops. Popping newest first floods one branch instead
of spreading.

## What it becomes

| Piece | Today | After |
|---|---|---|
| edges | separate bank, 8,192 total, 8 per slot, FIFO eviction (`record`, 45-79) | one edge per pair of depth-0 slots, holding the cheapest known tail between them; no count cap |
| memory | fixed reserve (`history_memory_bytes`, `archive.rs` 3073-3090) | each edge charged to the memory budget when recorded, the way the input index is; edges from slots with no remaining entries are dropped in `compact_history` (`archive.rs` 1375) |
| trigger | strictly preferred replacement only (`archive.rs` 2195-2200) | any replacement at a slot, preferred or cheaper at equal preference, since `insert_after` already replaces on both |
| queue | capped `VecDeque`, popped newest first (`improved` 81, `pop` 95) | one pending entry per edge keyed by `(from, to)`, updated to the latest parent, popped oldest first |
| tail length | `ACTION_CAP = 128` | the suffix shape's maximum, not a separate constant |
| share | one reservation in four (`campaign.rs` 2487; the replay check at 3569 must match) | one in two while anything is pending |
| enablement | `enable_continuations(config.mixture.uses_continuations())` (`campaign.rs` 2367, 3366) | always on |
| identifiers | `energy_splice_continuation_v1`, `_v2`, `alphabet_continuation_v1` in `draw.rs` 121-165 | removed; `energy_splice` and `alphabet_only` carry continuation; recorded runs naming the old ones fail to load with an error naming them; `ci.json` names `energy_splice_continuation_v1:6` and must change |
| accounting | dispatch totals | add pending edges, replays landing in the recorded destination slot, replays that replaced the destination's holder, and wave length, per progress record |

The dispatch loop at `campaign.rs` 2488-2530 stays as it is: skip stale
parents, bound the tail cost, skip fully archived prefixes, record the tail
as a splice record, pin the origin. Results pass through ordinary retention.

## Why this stays cheap

A replay continues past a slot only if the arriving state beats the holder.
Preference values are small bounded integers, so each slot improves a
bounded number of times over a run. Total replays are at most edges times
that bound. One pending entry per edge bounds the queue by the edge count.
The fixed share bounds the run time spent regardless.

Hot paths to keep small, and what "small" means for each:

| Path | Runs | Allowed work per call |
|---|---|---|
| `insert_after` records an edge | every admission | one map lookup and one insert on `(from, to)` |
| a replacement queues a slot's exits | every replacement | proportional to that slot's out-degree |
| a reservation pops one replay | every other reservation | constant |
| `compact_history` drops edges | rarely | proportional to the slots dropped |
| the memory charge | every admission | incremental add; never a full scan |

Anything on the first three rows that walks the whole edge set or the whole
archive is a defect. Check with the throughput comparison in `README.md`.

## Steps

### 1. Data structures

Replace `ContinuationBank` with:

- `edges: BTreeMap<(K::Group, K::Group), Edge<A>>` where `Edge` holds the
  tail, its cost, and the donor and leaf ids for the splice record.
- `exits: BTreeMap<K::Group, BTreeSet<K::Group>>`, the out-edges per slot.
- `pending: VecDeque<(K::Group, K::Group)>` plus
  `pending_parent: BTreeMap<(K::Group, K::Group), u64>` for dedupe and the
  latest parent.

`record(from, to, ...)` inserts or replaces only when the new tail is
cheaper. `improved(place, parent)` pushes each out-edge of `place` that is
not already pending and sets its parent. `pop()` takes from the front.

### 2. Memory

Charge each recorded edge's bytes at record time and release them at
compaction. Remove `reserve_bytes` and its use in `history_memory_bytes`.

### 3. Trigger and share

In `insert_after`, call `improved` on any replacement. In the dispatch loop,
change one in four to one in two, at both 2487 and 3569.

### 4. Always on, identifiers removed

Make `enable_continuations` unconditional and delete the three continuation
mixture identifiers, their `DrawMixture` variants, `isolates_continuations`
and `uses_continuations`. Keep the isolated accounting behaviour (replays do
not bump barren counters or reward the splice strategy), since that is now
the only behaviour. Update `benchmarks/search/ci.json` and any other manifest
naming a removed identifier.

### 5. Tail length

Tie the tail cap to the suffix shape's maximum and delete `ACTION_CAP`.

### 6. Tests

- Recording a cheaper tail for an existing pair replaces it; a costlier one
  does not.
- Two replacements at one slot in a row queue each exit once, with the
  latest parent.
- The queue pops oldest first.
- Dropping a slot's last entry under compaction removes its edges and any
  pending attempt on them, and releases their memory charge.
- A recorded stream replays bit-identically with continuation on.
- The generic resource fixture already exercises dispatch, eviction and
  replay under memory pressure; extend it to assert the charge grows with
  recorded edges and falls on compaction.

### 7. Docs

Rewrite the continuation paragraphs in `dissonance/searcher/README.md`
(around 165-200): what a wave is, what is stored per edge, that it is always
on, and the accounting fields.

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
