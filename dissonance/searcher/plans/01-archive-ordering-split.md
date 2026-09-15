# Step 1: split the key ordering into identity, progress and preference

Base: main. Line numbers from `58d07433`; refresh before editing.

## What is wrong

The searcher reads the derived `Ord` of `ArchiveKey::Group` as a measure of
how far along a lineage is. Field declaration order in a workload struct
therefore sets the strongest term in the selector, a 65,536-to-1 weight, and
the class walk hands every draw to the class that sorts highest until it has
nothing live. Moving one field in `MetroidArchiveGroup` with no change to any
key value doubled a boss area's coverage on one seed and changed which boss
died on another.

## What it becomes

Three questions, three answers, and nothing else reads a group as a
magnitude:

| Question | Answered by |
|---|---|
| Is this a new place? | `Eq` on the group. `Ord` stays only so maps can store it. |
| Is this band, class or leaf further along? | `ArchiveKey::progress_cmp`, default `Ordering::Equal`. |
| Which of two states at one slot survives? | `ArchiveKey::preference_cmp`, unchanged. |

`progress_cmp` must be a total preorder: for any three groups it is
transitive, and any two groups compare. In practice a workload writes it as
a comparison of a few numeric fields, and a workload with no progress notion
leaves the default. The searcher relies on this to take a maximum in one
pass instead of a dominance scan.

The class draw becomes a weighted draw over every class with a live cell.
The leading class takes the largest share and no live class takes zero.
Both parts are required: with the split alone the class walk still gives the
first live class every draw.

## Where the searcher reads the ordering today

All in `dissonance/searcher/src/search/archive.rs` unless stated.

| Site | Line | What it does | After |
|---|---|---|---|
| `progress_cmp` default | 36 | `left.cmp(&right)` | `Ordering::Equal` |
| `class_order` | 2485 | sorts classes by `Ord`, or a dominance scan under the semantic policies | replaced by the weighted class draw |
| `walk_to_cell_scan` | 2524, compare at 2569 | `band > *frontier` | `progress_cmp == Greater` |
| `walk_live_index` | 2622 | first class in `class_order` with a live cell | the weighted class draw |
| `deepest_live_band` | 2715, at 2729-2735 | `children.last()`, or a nested dominance scan | one pass taking the maximum by `progress_cmp`; ties keep the first in map order |
| `draw_group_index` | 2743, at 2812-2824 | `partition_point` by `Ord`, or a filtered scan | sort `ranked` by `progress_cmp` once, then `partition_point` with `progress_cmp`; span 16 unchanged |
| `deepest_leaf` update | 2296 | `previous >= (key, id)` on the full key | `leaf_order` (below) |
| `donors` set | 600, type at 606 | `BTreeSet<(K, usize, usize)>` ordered by the full key | a newtype whose `Ord` is `leaf_order` on the leaf, then leaf id, then donor id |
| `splice_tail_for_campaign` | 2334 | `leaf_key <= parent_key` | `!leaf_advances(leaf, parent)` |
| `recorded_splice_tail` | 2387 | `leaf_entry.key <= parent_key` | the same `leaf_advances` helper |
| `live_progress` update | 2270-2279 | `key > *deepest` and `key == *deepest` | `progress_cmp` on the cell group: `Greater` replaces, `Equal` keeps the lower cost |
| `semantic_progress` | 2477 | the switch between the two paths | deleted |
| `SelectorPolicy` | 106, identifiers 119-248 | nine variants | five: the three `EnergyFrontierCheapest*`, `Retire`, `GroupUniform` |

`leaf_order(a: (K, usize), b: (K, usize))` is `progress_cmp` on
`group(cell_depth())` of the two keys, then the entry id. It is a total
order because ids are unique. `leaf_advances(leaf, parent)` is
`leaf_order(leaf, parent) == Greater`, where `parent` is the parent entry's
key and id. `cell_depth()` is at 1764.

`deepest_key` in the campaign report (`campaign.rs` 2097, 2154) reads
`live_progress` and needs no change of its own.

## Steps

Do them in this order. Run the local checks from `README.md` after each.

### 1. Write the contract tests first

Put them in the `archive.rs` tests beside `LabelledKey` (5880), and follow
that test's pattern: build one archive per labelling, draw 12,000 times, and
count only draws whose `path` is `SelectorPath::HierarchyWalk`. The uniform
quarter of draws at 2419 picks entries in proportion to entry count and
would hide the effect. Each test must fail on the base commit; run it once
before changing searcher code and note the failure in the pull request.

1. **Labels do not change draws.** The same entries under two labellings
   where the labels reverse the `Ord` order and `progress_cmp` is unchanged.
   The per-class and per-band walk counts agree within the same tolerance
   the existing test at 5966 uses. Seeded sequences are not compared: map
   enumeration follows `Ord`, so identical sequences are not a goal.
2. **No class takes every draw.** Two classes, `progress_cmp` puts one
   strictly ahead. The leading class has one live cell, the other ten.
   Assert both classes receive walk draws and the leading class receives
   more. On the base commit the trailing class receives none.
3. **No progress notion means peers.** A key that leaves `progress_cmp` at
   its default: two bands that differ only in label receive walk counts
   within the tolerance above, under both labellings.
4. **Operation count.** A key with 64 live peer classes, then 128. Count
   `progress_cmp` calls per draw through a test-only counter on the key.
   Assert the count with 128 classes is at most twice the count with 64
   plus a constant, so the class draw is one pass and not a pairwise scan.

### 2. Change the trait

- `progress_cmp` default becomes `Ordering::Equal`.
- Add a test helper in `archive.rs` tests, `assert_total_preorder<K>(groups:
  &[K::Group])`, that checks transitivity over every triple. Every workload
  with a `progress_cmp` calls it from its own tests on a handful of groups.
- Nothing else on the trait changes.

### 3. Route every read through `progress_cmp`

Work down the table above. Where one representative is needed, take the
first maximal element in map order. Every scan that compared each element
against every other becomes a single pass or a sort; none remains.

### 4. Delete the switch and the duplicate policies

- Remove `semantic_progress()` (2477). Remove `persistent_key_counts()`
  (2470) only if nothing else reads it.
- Remove `SelectorPolicy::EnergyProgressCheapest`,
  `EnergyProgressCheapestCount`, `Energy` and `EnergyFrontier`, their
  identifiers, and the tests that only exercise them (3939, 3996, 5394,
  5527). Remove the `"energy"` arm in
  `workloads/nes/src/bin/metroid-campaign.rs:89`. No benchmark manifest
  names these four.
- Update every manifest under `benchmarks/search/*.json` that names a
  removed identifier: `ci.json` names
  `energy_progress_cheapest_count_v1`, and two more name
  `energy_progress_cheapest_v1`. Files under `benchmarks/search/results/`
  are records and stay as they are.
- A recorded run naming a removed identifier fails to load with an error
  that names the identifier.

### 5. Make the class draw a weight

In `walk_live_index` and `walk_to_cell_scan`, replace "loop over
`class_order`, return from the first class with a live cell" with one
weighted draw. It applies under every selector policy and needs no
threshold.

1. Collect the classes with at least one live cell, as `class_order` does
   at 2494-2501. This is one pass over `classes`, the same work
   `class_order` does today.
2. Sort them by `progress_cmp` descending. `rank` of a class is the number
   of distinct progress levels ahead of it, capped at 8. Peers share a rank.
3. `weight = 256 >> rank`.
4. Draw one class in proportion to weight with the archive's `rand`, so the
   draw is recorded and replays exactly.
5. Continue the walk inside that class as today.

With one class the draw is that class. With a one-depth key the class depth
is the slot depth and the draw runs over live slots; the continuation test
fixture in `campaign_continuation_tests.rs` has such a key and must keep
passing.

Put the per-class draw count into `SelectorAccounting` so a run report
shows the share each class received.

### 6. Workload edits

- `workloads/nes/src/metroid/archive.rs:67`: compare `items` only. Tanks
  are capacity and already live in the preference.
- `workloads/nes/src/mm2/archive.rs:130`: unchanged, `(bosses, boss_damage)`.
- `workloads/nes/src/smb/archive.rs:79`: SMB has no `progress_cmp` today
  and its progress came from the derived `Ord`. Add one comparing `(world,
  level, progress)` and nothing else. `group()` zeroes `progress` at depth 3
  and above, so the same comparison serves every depth. Add a test that a
  group further along in `progress` compares `Greater` regardless of
  `player_y_bucket`, `time_bucket`, `state_fingerprint` and `room`.
- Nova, Super Tilt Bro and the fault workloads keep the default and so move
  from field-order progress to no progress. Say so in each README.
- Each workload with a `progress_cmp` calls `assert_total_preorder` from a
  test.

The `area` field in `MetroidArchiveGroup` was already moved last on main.
After this step its position no longer matters. Leave it where it is.

### 7. Docs

Rewrite the "Search evaluation policies" section of
`dissonance/searcher/README.md` (161) for the shorter policy list, replace
the paragraph at 254-262 that describes the semantic experiment, and add a
short paragraph on the three questions, which trait method answers each,
and the total-preorder requirement. Check `docs/EXPLORATION.md` for the
removed policy names.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel. No long panel
yet. On the quick panel, expect Metroid to look different from today in
where entries sit, since the band rank inside a class is now flat and only
energy, cell recency and cost decide it. That is expected and is not a
reason to change the design. Read the per-class draw counts in the report
and confirm no class holds all of them after the first item is found.
