# Step 1: split the key ordering into identity, progress and preference

Base: main. Line numbers from `58d07433`; refresh before editing.

## What is wrong

The searcher reads the derived `Ord` of `ArchiveKey::Group` as a measure of
how far along a lineage is. Field declaration order in a workload struct
therefore sets the strongest term in the selector, a 65,536-to-1 weight, and
the class walk hands every draw to the class that sorts highest until it has
nothing live. Moving one field in `MetroidArchiveGroup` with no change to any
key value doubled a boss area's coverage on one seed and changed which boss
died on another. Moving the holdings fields produced a bit-identical run,
because the class walk never reads field order at all; it takes the top
class outright.

## What it becomes

Three questions, three answers, nothing else reads the group as a magnitude:

| Question | Answered by |
|---|---|
| Is this a new place? | `Eq` on the group. `Ord` stays only so maps can store it. |
| Is this band or class further along? | `ArchiveKey::progress_cmp`, default `Ordering::Equal`. |
| Which of two states at one slot survives? | `ArchiveKey::preference_cmp`, unchanged. |

And the class draw becomes a weighted draw over every class with a live
cell, with the leading class taking the largest share and no live class
taking zero. Both parts are required. With the split alone the class walk
still gives the first live class every draw, and a Metroid boss kill still
freezes the search inside the boss's area.

## Where the searcher reads the ordering today

All in `dissonance/searcher/src/search/archive.rs`.

| Site | Line | What it does |
|---|---|---|
| `progress_cmp` default | 36 | returns `left.cmp(&right)`, the derived `Ord` |
| `class_order` | 2485 | sorts classes descending by `Ord` unless the policy is semantic |
| `walk_to_cell_scan` | 2524, compare at 2569 | `band > *frontier` picks the deepest band by `Ord` |
| `walk_live_index` | 2622 | returns from the first class in `class_order` with a live cell |
| `deepest_live_band` | 2715, at 2735 | `children.last()` takes the max child by `Ord` |
| `draw_group_index` | 2743, at 2820 | `partition_point` counts bands above by `Ord`; weight at 2844 |
| `deepest_leaf` order | 2296, 2334 | full-key `>=` and `<=` decide the splice donor's leaf |
| `donors` set | 600 | `BTreeSet<DonorRank<K>>` ordered by the full key |
| `semantic_progress` | 2477 | the switch between the `Ord` path and the `progress_cmp` path |
| `SelectorPolicy` | 106, identifiers 119-248 | nine variants, two of them the semantic duplicates |

And in `dissonance/searcher/src/search/campaign.rs`: `deepest_key` in the
report at 2097 and 2154 is the max key by `Ord`.

## Steps

Do them in this order. Run the local checks from `README.md` after each.

### 1. Write the contract tests first

Put them in `dissonance/searcher/src/search/archive.rs` tests beside the
existing test keys (one with `progress_cmp` at 5898, one with
`preference_cmp` at 3773). Each test must fail on the base commit; run it
once before changing any searcher code and note the failure in the pull
request.

1. **Field order does not change draws.** Define one test key twice with the
   group struct's fields in two different orders and identical `group()`,
   `progress_cmp` and `preference_cmp`. Insert the same entries with the same
   seed into two archives, draw a few hundred times from each, and assert the
   sequence of selected entry ids is identical.
2. **No class takes every draw.** Build an archive with two classes where
   `progress_cmp` puts one strictly ahead. Give the leading class one live
   cell and the other class ten. Draw two thousand times and assert both
   classes were selected, and that the leading class was selected more
   often.
3. **No progress notion means peers.** A key that leaves `progress_cmp` at
   its default must draw identically under any field order, and a band that
   sorts higher by `Ord` must not receive more weight than one that sorts
   lower.

### 2. Change the trait

- `progress_cmp` default becomes `Ordering::Equal`.
- Nothing else on the trait changes. Do not add a method.

### 3. Route every read through `progress_cmp`

For each row of the table above, replace the `Ord` comparison with a
`progress_cmp` call. Ties are peers. Where one representative is needed
(`deepest_live_band`), take the first maximal element in map order so the
choice stays deterministic.

- `deepest_leaf` and `donors`: order by `progress_cmp` on the class-depth
  group, then by entry id. The splice check at 2334 becomes "the leaf's class
  is strictly ahead of the parent's class by `progress_cmp`, or equal and the
  leaf id is greater".
- `deepest_key` in the campaign report: the maximal key by `progress_cmp` on
  the class group, ties broken by entry id.

### 4. Delete the switch and the duplicate policies

- Remove `semantic_progress()` (2477). Remove `persistent_key_counts()`
  (2470) only if nothing else reads it.
- Remove `SelectorPolicy::EnergyProgressCheapest` and
  `EnergyProgressCheapestCount` and their identifiers. Their behaviour is now
  what `EnergyFrontierCheapest` and `EnergyFrontierCheapestCount` do. Also
  remove `Energy` and `EnergyFrontier` if no benchmark manifest or test names
  them; step 2 otherwise has to special-case them.
- Update every manifest under `benchmarks/search/*.json` that names a
  removed identifier (`ci.json` names `energy_progress_cheapest_count_v1`).
  Files under `benchmarks/search/results/` are records and stay as they are.
- A recorded run naming a removed identifier fails to load with an error
  that names the identifier.

### 5. Make the class draw a weight

In `walk_live_index` and `walk_to_cell_scan`, replace "loop over
`class_order`, return from the first class with a live cell" with one
weighted draw:

1. Collect every class with at least one live cell.
2. For each, `rank` = the number of other live classes that `progress_cmp`
   places strictly ahead of it, capped at 8.
3. `weight` = `energy >> rank`, where `energy` is the class's barren energy
   (see below), with a floor of 1.
4. Draw one class in proportion to weight, using the archive's `rand` so the
   draw is recorded and replays exactly.
5. Continue the walk inside that class as today.

Class energy: `group_barren` (554) holds one map per pooled depth, sized
`groups() - 2`, so the class depth has no counter. Extend it by one map so
the class depth has a barren counter like the depths below it, bumped on
every selection at 3217 and cleared in `record_selection_outcome` (3236)
under the same rule as the other depths. The weight then falls for a class
that keeps producing nothing, which is what stops a post-kill class from
holding the top share forever.

Put the per-class draw share into the selector report so it can be read
from a run.

### 6. Workload edits

- `workloads/nes/src/metroid/archive.rs:67`: compare `items` only. Tanks are
  capacity and already live in the preference, so classes that differ only
  in tanks share weight.
- `workloads/nes/src/mm2/archive.rs:130`: unchanged.
- Nova, Super Tilt Bro and the fault workloads keep the default and so move
  from field-order progress to no progress. Say so in each README.

The `area` field in `MetroidArchiveGroup` was already moved last on main.
After this step its position no longer matters. Leave it where it is.

### 7. Docs

Rewrite the "Search evaluation policies" section of
`dissonance/searcher/README.md` (161) for the shorter policy list, and add a
short paragraph on the three questions and which trait method answers each.
Check `docs/EXPLORATION.md` for the removed policy names.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel. No long panel
yet. On the quick panel, expect Metroid to look different from today in
where entries sit, since the band rank inside a class is now flat and only
energy, cell recency and cost decide it. That is expected and is not a
reason to change the design. Read the per-class draw share in the report and
confirm no class holds all of it after the first item is found.
