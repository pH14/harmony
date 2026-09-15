# Step 2: reset energy only at the depths where a descendant is new

Base: the step 1 merge commit. Line numbers from `58d07433`; refresh before
editing, and expect `record_selection_outcome` to have changed in step 1.

## What is wrong

Energy is the per-depth barren counter that lowers a place's draw weight
when draws from it produce nothing. Each selection bumps the parent's
counter at every pooled depth. A productive draw clears them all. Productive
today means the child landed in a depth-1 group (a 32-pixel cell in the NES
workloads) the archive had never seen.

So a new 32-pixel cell inside a room the search already covers counts as
full credit for the room and its region. A room has a few hundred possible
cells once posture and door state are multiplied in, and every tank pickup
creates a fresh copy of all of them. Covered rooms keep producing this fine
novelty and never fade. In the Metroid controls, selection cells grew from
22,000 to 89,000 while map cells froze at 262; in Mega Man 2 the first stage
screen held 13 percent of the archive.

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
| 4, class (added in step 1) | new 32-pixel cell | new class |

This removes the advantage covered places hold. It gives the frontier no
bonus of its own; that comes from cell recency and from step 3.

## Where it lives

All in `dissonance/searcher/src/search/archive.rs` unless stated.

| Site | Line | Today |
|---|---|---|
| `opened_cell: Vec<bool>`, `cells_seen: BTreeSet<K::Group>` | 546-547, 1038-1039 | one seen-set at depth 1, one bool per entry |
| compaction of both | 1444, 1456 | rebuilt with the retained entries |
| the new-cell computation in `insert_after` | 2286-2291 | `cells_seen.insert(key.group(1))` |
| `opened_new_cell(id)` | 2995 | returns the bool |
| `record_selection_outcome` | 3236, clear loop at 3262 | clears every `group_barren` map for the parent's groups |
| callers in `campaign.rs` | 2902, 3801 | `any(opened_new_cell)` over the retained children |
| `counter_resets` | 294 | the walk's own counter; unrelated, leave it |

## Steps

### 1. Record which depths a new entry opened

In `insert_after`, replace the single seen-set and bool with one seen-set
per pooled depth and one `u8` mask per entry, bit `d` set when `group(d)`
was new:

```rust
let mut opened = 0u8;
for depth in 1..K::groups() {
    if self.groups_seen[depth - 1].insert(key.group(depth)) {
        opened |= 1 << depth;
    }
}
self.opened_depths.push(opened);
```

Keep `cells_seen.len()` available as the depth-1 entry of the new vector; it
feeds the cell count in reports (3107, 3134). Compaction rebuilds every
seen-set the way it rebuilds `cells_seen` today. `opened_new_cell(id)`
becomes `opened_depths(id) -> u8`.

If `opened_cell` is written into the archive checkpoint, the checkpoint
format changes. Older checkpoints then fail to load with a clear error;
that is acceptable.

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

If step 1 kept the `Energy` or `EnergyFrontier` policies, which clear on a
new slot rather than a new cell, treat a new slot as all bits set for them.

### 4. Count resets per depth

Add `energy_resets: [u64; N]` (one per barren map) to the selector
accounting and report it. It tells a reader of a run what share of
map-cell resets came from a child that was new only at depth 1, which is
the number that says whether this change mattered on that run.

### 5. Tests

- Update the existing calls that pass three bools to
  `record_selection_outcome` (5097, 5135, 5323, 5371) to pass a mask.
- New: a child new at depth 1 only zeroes the parent's depth-1 counter and
  leaves depths 2 and 3 as they were.
- New: a child new at depth 3 zeroes depths 1, 2 and 3.
- New: the accounting reports one reset per cleared counter per depth.
- Replay identity: an existing recorded stream still replays exactly, since
  the mask is derived from the same admissions in the same order.

### 6. Docs

One paragraph in `dissonance/searcher/README.md` under the energy policy
description saying that a counter clears only when the child is new at that
counter's depth.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel. Expect most
map-cell counters to sit at the floor most of the time now, since the
halving thresholds `3,6,12` were tuned when resets were frequent. That is
expected. If the quick panel shows Metroid or Mega Man 2 reaching less far
on every seed, the first thing to try is a larger depth-3 threshold in the
selector identifier, and the reset rule stays. Read `energy_resets` per
depth in the reports and put the numbers in the pull request.
