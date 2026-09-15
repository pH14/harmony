# Step 2: reset energy only at the depths where a descendant is new

Base: the step 1 merge commit. Line numbers from `58d07433`; refresh before
editing, and expect `record_selection_outcome` and the selector policy list
to have changed in step 1.

## What is wrong

Energy is the per-depth barren counter that lowers a place's draw weight
when draws from it produce nothing. Each selection bumps the parent's
counter at every pooled depth. A productive draw clears them all. Productive
today means the child landed in a depth-1 group (a 32-pixel cell in the NES
workloads) the archive had never seen.

So a new 32-pixel cell inside a room the search already covers counts as
full credit for the room and its region. Covered rooms keep producing this
fine novelty and never fade. In the Metroid controls, selection cells grew
from 22,000 to 89,000 while map cells froze at 262; in Mega Man 2 the first
stage screen held 13 percent of the archive.

## What it becomes

A counter at depth `d` is cleared only when the child's group at depth `d`
is new. A new map cell implies a new region and a new cell, so it still
clears all three. A new cell inside an old room clears only depth 1. The
searcher never names a workload's depths; it asks each group whether it has
been seen before.

| Depth (Metroid) | Cleared today by | Cleared after by |
|---|---|---|
| 1, 32-pixel cell | new 32-pixel cell | new 32-pixel cell |
| 2, 128-pixel region | new 32-pixel cell | new region |
| 3, map cell | new 32-pixel cell | new map cell |

The selector identifier `hierarchy_uniform_128_energy_frontier_cheapest:3,6,12,2`
reads as entry threshold 3, then one halving scale per pooled depth: 6 at
depth 1, 12 at depth 2, 2 at depth 3 (`archive.rs` 218-232).

`Retire` clears every depth on any productive selection today and keeps
doing so. `GroupUniform` never clears.

## Where it lives

All in `dissonance/searcher/src/search/archive.rs` unless stated.

| Site | Line | Today |
|---|---|---|
| `opened_cell: Vec<bool>`, `cells_seen: BTreeSet<K::Group>` | 546-547, 1038-1039 | one seen-set at depth 1, one bool per entry |
| compaction of both | 1444, 1456 | rebuilt with the retained entries |
| the new-cell computation in `insert_after` | 2286-2291 | `cells_seen.insert(key.group(1))` |
| `opened_new_cell(id)` | 2995 | returns the bool |
| `novelty_memory_bytes` | 3106 | charges `cells_seen.len()`; feeds `history_memory_bytes` (1732) and so compaction (1375) |
| `historical_cell_count` | 3133 | `cells_seen.len()` for reports |
| `record_selection_outcome` | 3236, clear loop at 3262 | clears every `group_barren` map for the parent's groups |
| callers in `campaign.rs` | 2902, 3801 | `any(opened_new_cell)` over the retained children |
| `CAMPAIGN_SCHEMA_VERSION` | `campaign.rs` 46 | 2; replay refuses any other value (3214) |
| `counter_resets` | 294 | the walk's own counter; unrelated, leave it |

## Steps

### 1. Record which depths a new entry opened

In `insert_after`, replace the single seen-set and bool with one seen-set
per depth from 1 to `groups() - 1` and one `u32` mask per entry, bit `d`
set when `group(d)` was new:

```rust
let mut opened = 0u32;
for depth in 1..K::groups() {
    if self.groups_seen[depth - 1].insert(key.group(depth)) {
        opened |= 1 << depth;
    }
}
self.opened_depths.push(opened);
```

`insert_after` returns an error if `K::groups() > 32`; the trait sets no
bound of its own. With `groups() == 1` there is no seen-set, bit 0 of the
mask is set when the slot is new, and `historical_cell_count` returns the
slot count, which is what the `new_slot` fallback at 2287-2290 reports
today. `historical_cell_count` otherwise returns the length of the depth-1
set. `novelty_memory_bytes` charges every set, each length times
`historical_group_memory_charge(0)`. Compaction rebuilds every set the way
it rebuilds `cells_seen` today. `opened_new_cell(id)` becomes
`opened_depths(id) -> u32`.

### 2. Combine over the children

At both `campaign.rs` call sites, fold a bitwise OR of `opened_depths` over
the retained children in place of `any(opened_new_cell)`, and pass the mask
to `record_selection_outcome` in place of the bool.

### 3. Clear only those depths

In `record_selection_outcome`, the clear loop gains one condition:

```rust
for (offset, map) in self.group_barren.iter_mut().enumerate() {
    if opened & (1 << (offset + 1)) != 0 {
        map.insert(key.group(offset + 1), 0);
    }
}
```

For `Retire`, pass a mask with every bit set.

### 4. Bump the schema version

The memory charge changed, so compaction can fire at a different moment
than a stream recorded before this step expects. Set
`CAMPAIGN_SCHEMA_VERSION` to 3. Older streams then fail with the existing
schema error; the test at `campaign.rs` 5378 covers that path.

### 5. Count resets and what caused them

Add to `SelectorAccounting`:

- `energy_resets: Vec<u64>`, one per barren map, the counters actually
  cleared at each depth.
- `productive_by_mask: BTreeMap<u32, u64>`, a histogram of the mask over
  productive selections. Few distinct masks occur. Before this step every
  mask with bit 1 set cleared every depth; after it only the depths in the
  mask clear. So the share of old-style full resets this change removed on
  a run is the count at mask `2` (bit 1 alone) divided by the sum over
  masks with bit 1 set.

### 6. Tests

- Update the existing calls that pass three bools to
  `record_selection_outcome` (5097, 5135, 5323, 5371) to pass a mask.
- New: a child new at depth 1 only zeroes the parent's depth-1 counter and
  leaves depths 2 and 3 as they were.
- New: a child new at depth 3 zeroes depths 1, 2 and 3.
- New: the accounting reports one reset per cleared counter per depth, and
  `productive_by_mask` counts the depth-1-only mask separately from a mask
  with bits 1 and 3.
- New: `novelty_memory_bytes` grows when a group is new at depth 2 only.
- Replay: the generic resource fixture in
  `campaign_continuation_tests.rs` runs a live campaign under a memory
  budget tight enough to compact, replays its stream, and asserts the
  replayed archive matches the live one entry for entry. Extend it with a
  key of three depths so the extra seen-sets are charged and compacted.

### 7. Docs

One paragraph in `dissonance/searcher/README.md` under the energy policy
description saying that a counter clears only when the child is new at that
counter's depth, and naming the two new accounting fields.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel. Expect most
map-cell counters to sit at the floor most of the time now, since the
depth-3 scale of 2 was set when resets were frequent. That is expected. If
the quick panel shows Metroid or Mega Man 2 reaching less far on every
seed, the first thing to try is a larger depth-3 scale in the selector
identifier, recorded as its own manifest change; the reset rule stays. Put
`energy_resets` and `productive_by_mask` per seed in the pull request.
