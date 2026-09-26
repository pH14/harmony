<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Tiny deterministic worlds

Tiny worlds exercise Dissonance's campaign engine with bounded state machines.
Each world supplies transitions, snapshots, archive keys, action sampling, and an
objective. Exhaustive reachability checks and direct action replay provide
independent checks of campaign results.

## Running a world

Build the executable with:

```sh
cargo build --release --locked --manifest-path workloads/tiny-worlds/Cargo.toml
```

The executable reads one JSON request from standard input, capped at 16 KiB.
Required fields are `config`, `seed`, `work_budget`, `broken`, `verify`, and
`keep`; `scale` is optional.
`config` contains `family` and `parameters`; nested objects reject unknown fields.
Supply a seed at runtime and retain it with the output when reproducing a run.
Keep generated requests and reports outside the repository.

For example, this command generates a request at runtime:

```sh
python3 - <<'PY' | workloads/tiny-worlds/target/release/tiny-worlds
import json, secrets
print(json.dumps({
    "config": {"family": "maze", "parameters": {
        "length": 4, "pattern": 10, "reverse_actions": False
    }},
    "seed": secrets.randbits(64),
    "work_budget": 1000,
    "broken": False,
    "verify": True,
    "keep": "portfolio"
}))
PY
```

`keep` selects slot retention. `portfolio` keeps one holder per slot under two
preferences: charge first and health first. `capacity_two` keeps two holders
per slot under the charge-first preference alone.

`work_budget` accepts 1–2,000,000 transitions. A campaign uses one worker, one
reservation, an archive capacity of 4,096, and a 32 MiB logical memory budget,
and stops at its first objective. The event stream has a checked 1 GiB
allocation limit. Execution work counts
restored-parent replay and suffix actions; restoring a snapshot preserves the
cumulative work counter. Environmental clocks belong to the restored world state.

The JSON report contains the request parameters, build-time source hashes,
objective result, execution work, archive statistics, and family diagnostics.
With `verify=true`, the runner replays the campaign stream, repeats execution with
the same runtime seed, checks any objective witness against world transitions,
independently sums admitted job work, and checks that the last job started
before the work budget was spent. Tests check these invariants using
runtime-generated seeds; controlled transition and archive tests check the
mechanics directly.

## Scaled runs

An optional `scale` object replaces the fixed campaign settings, to measure
searcher throughput and memory at sizes the other runs cannot reach. Every
field is required:

| Field | Bounds | Meaning |
| --- | --- | --- |
| `workers` | 1–16 | Worker threads. |
| `reservations_per_worker` | 1–8 | Admission window per worker. |
| `results_per_worker` | 1–2 | Finished results a worker may hold before admission: `ResultBuffering::OnePerWorker` or `TwoPerWorker`. |
| `memory_budget_mib` | 1–16,384 | The searcher's logical memory budget. |
| `archive_entries` | 1–4,194,304 | Archive entry limit. |
| `action_cost_ns` | 0–10,000,000 | Thread CPU time each transition spins on its worker thread, so a descheduled worker takes longer, as a real target would. |
| `snapshot_bytes` | 0–1,048,576 | Pseudorandom payload stored in every snapshot and charged to the memory budget. |

The payload is derived from the world state, so it adds real resident memory
without changing any search decision, and job-result hashes leave it out. The
spin reads the thread's CPU clock only to wait. A scaled run accepts 1–10,000,000,000
transitions and requires `verify=false` and `keep=portfolio`. It counts the
campaign stream's bytes instead of storing them, keeps no per-job evidence,
skips final archive entries, and writes the searcher's progress lines, one per
100 executions, to standard error. `HARMONY_COORDINATOR_PROFILE` adds the
coordinator's time profile to standard error. The report on standard output
holds the settings, the objective result, work, elapsed time, stream bytes,
logical resident memory, live entries, and selector counters.

## Families and controls

`broken=true` selects the control listed below. Controls expose how a particular
representation or action choice affects the same world transitions.

| Family | Mechanics | Control |
| --- | --- | --- |
| `resource` | Refill charge, preserve health, and spend charge along a corridor and at its barrier. | Hide charge from the archive key. |
| `maze` | A sequence of choices determines whether the endpoint opens; a reset permits another attempt. | Omit choice history from the archive key. |
| `actions` | Traverse land and water stages with different effective actions. | Sample only the land action. |
| `deadline` | Choose slow single-action or fast two-action travel before paying a final time cost. | Hide remaining time from the archive key. |
| `delayed` | Complete a repeated sequence or wait while distraction lanes compete for exploration. | Omit partial progress from the archive key. |
| `deadline_actions` | Traverse changing-action stages while every action spends environmental time. | Sample only the land action. |
| `chain` | Complete an ordered sequence of leaf worlds in one campaign. | Omit stage identity, or carried stock when charge carries between stages. |
| `route` | Scout a route, return for an upgrade, and traverse it again. | Uses the same representation in both settings. |
| `backtrack` | Walk a line whose barriers each need one more item; the next item is taken at the hub after reaching the next barrier. | Hide items from the key. |
| `trap` | Follow a corridor to the goal, or enter a side corridor to an item whose rooms never reach the goal. | Hide the item from the progress tier. |
| `map` | Enter a branch of a grid of rooms, take the item at its far end, leave through the same door, and cross the rest of the map to the goal. | Hide the item from the progress tier. |
| `graph` | Walk a long line of nodes, with hashed jumps, to its last node. | Hide the progress tier. |

Resource keys retain charge-first and health-first preferences. Deadline keys
retain the partial fast-route phase and prefer remaining time. Exact payment of
an obstacle's time cost succeeds. In `deadline_actions`, ineffective actions
also spend their configured duration. The action world's `observable` flag
controls whether regime identity appears in the key.

## Archive key

The key's progress is the pair (goal, tier). The searcher ranks slots by
progress, so a higher tier draws most of the selection weight. Leaf families
set tier 0 except where an option below raises it.

Delayed progress uses `sequence` or `wait` mode. `placement` puts partial
progress in the key: `identity` as context, `place` as part of the place,
`engaged` in the place with tier 1 once progress is nonzero, and `tier` as the
tier itself. With `sticky_credit=true`, leaving the fight lane keeps the last
reading in the key of every distraction lane; it requires a placement other
than `identity`. With `ammo` nonzero, every fight action spends one ammo, a
miss keeps progress, and a hit needs ammo. Distraction action 3 refills one
ammo up to the maximum. Leaving the fight lane resets progress. Ammo appears
as charge, which preferences favour.

The trap world's item raises the tier to 1 and makes the goal unreachable;
reachability checks every item room. Route `ranked_upgrade=true` puts the
upgraded phase in tier 1.

The backtrack world places barriers every `segment` positions along a line of
`(barriers + 1) * segment` positions. Passing barrier k needs k items. Reaching the
next barrier marks it scouted; action 3 at the hub then takes one item. A wrong
route action returns to the hub and keeps items. Each stage therefore walks
out, returns, and crosses the earlier ground again. `placement` puts items in
the `tier`, in the slot `identity`, or only in a retention `preference` through
charge. The report's `backtrack_first_items` gives the first work at which
each item count was held.

The map world is a `width` by `height` grid of rooms. Doors form a spanning
tree drawn by a randomized depth-first walk from room 0, seeded by `layout`,
so the map has long corridors, branches, and dead ends. The inner region is
the tree branch of about `inner` rooms that hangs from one door; the campaign
starts in room 0 in the outer region, and that door is the only way in or
out. Up to `loops` extra doors join rooms within one region. The item is the
inner room farthest from the door, and the goal is the outer room farthest
from the door; reaching the goal counts only while holding the item, so a
campaign enters the inner region, takes the item, leaves through the same
door, and crosses the outer region again. Actions 0–3 are up, right, down, and
left. Crossing a horizontal door takes `corridor` presses in its direction and
crossing a vertical door takes `shaft` presses; the opposite action steps back
and the side actions do nothing. The archive place is the room and the
position inside its doorway, and the tier is 1 once the item is held. Map
reports include the `layout` (doors per room, regions, item, key, door, goal,
region entry, farms, ordered items, and room distances; item and key are null
when absent), `evidence.map_first` (first work entering the inner region,
holding the item, leaving it with the item, and at the goal),
`evidence.map_first_tier` (first work at each tier), `evidence.map_first_stocked`
(first work arriving in a boss room holding the item and enough stock, and
enough health when the boss hits back), and `parent_timeline`, which lists
each job's start work, its parent's tier, and its parent place in order. A room
is its place divided by 16; boss hit counts are places 1,024 plus the hits.
As a known limit, the map spreads draws faster than Metroid. After the item
it leaves the item room at once and settles with 0.8–0.9 of top-tier draws
outside the inner region, where Metroid stays on 6–12 map cells for about 0.4
of the first trip and settles at 0.3–0.5.

Optional map fields add Metroid behaviours; each defaults to off. The search
key puts `stock` in the stock field and `health` in the health field, so both
feed the slot preferences and neither changes the tier.

| Field | World | Metroid behaviour |
| --- | --- | --- |
| `farms`, `farm_cap` | `farms` farm rooms; a layout whose region has too few rooms is rejected. Entering an even-numbered farm adds one health and an odd-numbered farm one stock, up to `farm_cap`. Without a boss the farms are inner rooms other than the item room. | Refilling energy and missiles in rooms that do not lead to the next item: a stock gain wins the slot preference and draws the search back to the farm. |
| `items` above 1 | The items are outer rooms, taken in order, each the room farthest from the start and the earlier items. The door to the inner region opens once all are held, and the goal is the inner room farthest from the door; the tier is the item count. | Each item gain sends a new tier back across the whole map to the one region it opens. |
| `boss_stock` | A boss in the goal room, which is the outer room farthest from the door among rooms with at most three doors, so a wall remains to press. Needs `farms` of at least 2 and `boss_stock` at most `farm_cap`. Stock farms fill only while holding the item and lie in the outer half farthest from the goal. Each wall press in the goal room with the item spends one stock and adds a hit; any step into a doorway resets hits. The goal needs `boss_stock` hits. Hit counts are their own places. | Kraid and Ridley: the item tier must farm missiles far away, then carry them to the boss room. |
| `boss_hits_back` | Needs `boss_stock` 1–6. Each hit also spends one health and needs health left, each stock farm entry spends one health, and health stops at `boss_stock` plus 2. The goal needs `boss_stock` stock and `boss_stock` health on one arrival. | Arriving at a boss with enough missiles or enough energy but not both: the preference that keeps the most stock keeps a holder low on health. |
| `item_optional` | The inner region is the shallowest side branch whose size is within two rooms of `inner`, or the branch closest in size when none is, and the item is its room farthest from the entry. The goal is the outer room farthest from the start that is not on the way to that branch. The goal counts without the item, which still raises the tier. | Varia and other pickups off the main path: the new tier walks back over ground the lower tier already reached. |
| `locked` | The inner region is chosen as for `item_optional`. Its door opens only while holding a key in the outer room farthest from the start other than the door room; the goal is the outer room farthest from the door other than the key room and needs the item. The key raises the tier to 1 and the item to 2. | Varia in Brinstar after the lower tier reached the Tourian shaft: the lower tier's frontier is far from the pickup, and the new tier must walk back across the map. |
| `timing` | 0, or a period of 2–16. Every action is rotated by a hidden phase that advances by one each step modulo `timing`, so a recorded suffix replayed from a holder with another phase takes different actions. At 5, about one replayed suffix in five lands, and at 10 about one in ten. A period that is a multiple of 4 lands one in four, because rotations by phases four apart are equal. | Enemy positions and frame timing that the key does not see. |

The graph world is sized for scaled runs. It has `nodes` states in a line,
each also carrying stock and health from 0 to 15. Action 0 moves to the next
node, so the goal at the last node is reachable from every state; the
reachability check holds by this construction instead of enumeration. Actions
1–3 jump to a node drawn from a hash of `layout`, the node, and the action, and
add amounts from the same hash to stock and health modulo 16. The place splits
the nodes into `places` equal ranges and the identity is the node's position in
its range, so a large graph gives hundreds of thousands of slots in about as
many cells as `places`. Stock and health feed the two slot preferences, and the
tier splits the nodes into `levels` equal ranges. Graphs cannot be chain
stages.

Diagnostics count admitted suffix observations. Arrival histograms count
transitions into a location; resource refill counts require a stock increase.
Delayed-progress histograms include distraction observations in their zero bin.
Exported archive entries include ancestry; `live_entries` reports active holders.
Continuation job counts and work come from the campaign stream.
`parent_draws` counts jobs by (before objective, selector path, tier rank,
parent tier, parent place). `skipped_draws` counts draws whose parent and
suffix were already executed; they produce no job.

## Configuration bounds

Every parameter is required except the optional map fields. Bounds keep
exhaustive enumeration below 200,000 states; requests exceeding the reachability limit are rejected. The graph
family's reachability holds by construction.

| Family | Parameters |
| --- | --- |
| Resource | `initial_charge` 0–31; `initial_health` 1–31; `barrier_charge` 1–15; `route_cost`, `health_cost`, and `refill_health_cost` 0–15; `refill_amount` and `max_charge` 1–31; `corridor_len` 1–12; `initial_charge` ≤ `max_charge`. |
| Maze | `length` 1–8; `pattern` < `1 << length`; boolean `reverse_actions`. |
| Actions | `segment_len` 1–8; `water_action` 1–3; boolean `return_to_land` and `observable`. |
| Deadline | `length` 1–8; `initial_time` 1–64; `fast_ticks` 1–4; `slow_ticks` > twice `fast_ticks` and ≤ 16; `obstacle_ticks` 1–16. |
| Delayed | `horizon` 1–16; `distractions` 1–32; `mode` is `sequence` or `wait`; `placement` is `identity`, `place`, `engaged`, or `tier`; boolean `sticky_credit`; `ammo` 0 or `horizon`–31. |
| Deadline/actions | `actions` uses the actions parameters; `initial_time` 1–64; four `action_ticks` values, each 1–16. |
| Route | `length` 2–16; `pattern` encodes two-bit actions per position; `attack` 0–3; boolean `shifted`, `upgrade_required`, and `ranked_upgrade`. |
| Backtrack | `barriers` 1–4; `segment` 1–8; `(barriers + 1) * segment` ≤ 32; `pattern` encodes two-bit actions per position and its first action differs from 3; `placement` is `tier`, `identity`, or `preference`. |
| Trap | `length` 1–16; `pattern` encodes two-bit actions per position and its first action differs from 3; `trap_len` 1–8; `rooms` 1–16. |
| Map | `width` and `height` 2–8; any `layout`; `loops` 0–16; `corridor` and `shaft` 1–4; `inner` from 2 to two fewer than the room count; `items` 1–9, and above 1 only without farms and with two outer rooms to spare; `farms` 0–8 with `farm_cap` 1–63, and `farm_cap` only with farms; `boss_stock` 0–`farm_cap`, 1–6 with `boss_hits_back`, and not in chain stages; `timing` 0 or 2–16; `item_optional` needs one item and no boss; `locked` needs one item, no optional item, no boss, and an outer room for the key besides the start and the door room. |
| Graph | `nodes` 16–4,194,304; `places` 1–`nodes` with at most 65,536 nodes per place; `levels` 1–16; any `layout`. |

## Scenario chains

A chain has one to sixteen `stages`, each containing a leaf `world` and
`refill_available`. Every family except `chain` and `graph` is a supported leaf. Completion enters the next stage in the same
action. The final stage's goal is the campaign objective. Snapshots contain the
active stage, local state, and carried charge. Health, history, and clocks reset
on stage entry; archived snapshots allow exploration from earlier stages.

With `carry_charge=false`, each stage starts from its declared initial state and
chain `initial_charge` is zero. Stage identity namespaces archive places in
blocks of 1,024. With `ranked=true`, a stage's tier is its leaf tier plus 32
times the stage index, so a later stage outranks every earlier one; the control
that omits stage identity keeps only the leaf tier. With
`ranked=false`, every chain key has tier 0. With
`carry_charge=true`, chain `initial_charge` initializes persistent charge;
resource stages consume and replenish it, while other stages preserve it.
Resource stages share a capacity and declare local `initial_charge=0`.
`refill_available=false` disables the resource refill action. Other leaves
require this flag to be true.

Chains use a uniform four-action alphabet. Across stage namespaces,
preferences compare persistent stock; within a stage they also compare local
resources. Whole-chain reachability is checked independently of each stage's
reachability.

Chain diagnostics include stage-entry work and charge distributions, suffix
actions per stage, and executed work grouped by parent stage and charge.
Pre-objective counters include the objective job and are checked against work to
the first objective, or total work for an unfinished campaign.

## Upgrade route

The route world requires scouting a blocked endpoint before `attack` returns to
the origin and acquires an upgrade. Wrong route actions return to the origin
while preserving the upgrade. With `shifted=true`, acquisition changes lanes;
`(attack + 1) % 4` aligns the player before route movement succeeds. Alignment
must differ from the first route action. With `upgrade_required=false`, the
first traversal completes the objective.

Position and lane identify archive places; phase is a retention preference.
Route reports join executed observations to admitted campaign jobs. They record
first upgraded position/lane arrivals, acquisition, alignment, and continuation
transfers that advance an upgraded state. Donor and leaf states come from
historical admissions. A `non_upgraded_donor` label describes their recorded
phase. Job-end work includes replay overhead. The action mixture draws random
one- or two-action suffixes and supports continuation dispatch.

## Searcher panel

The panel runs the worlds under settings whose outcomes match recorded Metroid
search behaviour, then checks each outcome's direction:

```sh
uv run workloads/tiny-worlds/panel.py
```

It builds the executable, draws every seed at runtime, runs six seeds per arm
(twelve for the engaged and level-as-tier tight-ammunition boss arms) in six
processes, and prints one PASS or FAIL line per rule. It writes no files. A run
takes under a minute on a ten-core laptop with `--jobs 10`. `--seeds` and
`--jobs` change the sample and parallelism; `--binary` uses a prebuilt
executable instead of the fresh build.

| Rule | Setting |
| --- | --- |
| Boss damage | Delayed boss with tight or ample ammunition; partial progress in the place, as an engaged tier, or as the tier itself. |
| Credit kept after leaving | Engaged boss with and without `sticky_credit`. |
| Unwinnable top rank | Trap world against its control, including the top-tier draw share, which follows the selector's tier rank weight. |
| Retention | Two-stage chain under `portfolio` and `capacity_two`; the rule expects equal work. |
| Backtracking | Backtrack world with items ranked, split by identity, kept as a preference, or hidden. |
| Chains | Sixteen-stage chains: flat, ranked stages, ranked stages with ranked upgrades, and kept boss credit. |
| Growing gaps | Eight-by-eight map with the item ranked or hidden, one layout per seed; the return trip is shorter than the first trip, and per room of path the walk from the door to the goal takes longer than the return trip. |
| Farm loop | Eight-by-eight map with `inner` 20 and 4 farms against the same layout and seed without farms, 200,000 work each. The median ratio of the trip from leaving the item region to the goal stays under 10x, which six-seed medians on the current searcher stayed below in 20,000 resamples of 300 pairs. The current searcher measures 5.87x. Resetting a cell's draw count only when a carried-in holder reaches a higher power of two of stock or health measures 2.30x, and 1.88x with a 16x cell weight for places no lower tier reached; with that reset rule in the searcher the threshold is 3.5x. |

Timing rules compare medians in which an unsolved run counts as the work
budget on the side that must be slower and as unbounded on the side that must
be faster, so a rule passes only when solved runs establish it. A milestone
reached after the budget counts as unreached. Count
allowances scale with the seed count and round up. Each rule records current
searcher behaviour. A searcher change that flips a rule predicts the same
change on Metroid. With six seeds per arm, a rule close to its threshold can
flip between runs of an unchanged searcher, so rerun a FAIL before attributing
it to a change. The panel is not a CI check.

### Comparing a searcher change

`--compare BASELINE` also runs the Metroid worlds on the baseline executable and
on `--binary` or the fresh build, with the same layouts and runtime seeds on both:

```sh
uv run workloads/tiny-worlds/panel.py --jobs 10 --binary candidate --compare baseline
```

| World | Settings | Metroid behaviour | Legs |
| --- | --- | --- | --- |
| Farm loop | `inner` 20, 4 farms | Refills away from the next item draw the search back | To the item, out of the item region, out to the goal |
| Whole-map re-walk | `inner` 4, 9 items | Each item sends a new tier back across the map | To the last item, last item to the goal |
| Boss needing far stock | `inner` 20, 4 farms, `boss_stock` 24 | Kraid and Ridley need missiles farmed far from the boss | To the item, item to stocked arrival, stocked arrival to kill |
| Boss needing stock and health | `inner` 20, 4 farms, `boss_stock` 6, `boss_hits_back` | Arriving with enough missiles or enough energy but not both | As above |
| Off-path item | `inner` 6, `item_optional` | A new tier from an off-path pickup walks back over reached ground | Pickup to the goal |
| Off-path item with farms | as above, 2 farms | The same with refills | Pickup to the goal |
| Locked item | `inner` 4, `locked` | The new tier's lower-tier frontier is far from the pickup | To the key, key to the item, item to the goal |
| Locked item with hidden timing | as above, `timing` 5 | Replayed inputs land about one time in five | As above |

Every world also reports work to the goal, counting a missed goal as 200,000.
A campaign stops at the goal or after 200,000 work, and a milestone after that
counts as unreached. Comparison runs skip replay verification; the rule panel
verifies every run. The first four worlds run `--world-scale` layouts
(default 16) and the other four run four times as many. A stocked arrival is
the first arrival in the boss room holding the item and at least `boss_stock`
stock, and at least `boss_stock` health when the boss hits back. For each leg the panel prints the median over layouts of the
candidate-to-baseline ratio of work plus one, a 99% bootstrap interval, and how
many layouts reached the leg's end in each run; only layouts where both runs
reached it enter the ratio, and a leg with fewer than three such layouts
prints nan. The off-path pickup-to-goal leg covers only layouts
where the pickup came before the goal, and a change in that count changes which
layouts it compares. A leg is slower when the interval lies above 1.25 and
faster when it lies below 0.8. A world is clearly bad when any leg is slower or
the candidate misses the goal on two more layouts than the baseline. The exit
status is nonzero when a rule fails or a world is clearly bad. Two unchanged
searchers with different runtime seeds gave no slower or faster leg in 300
trials per world. Each world also counts the layouts whose event streams are
identical on both executables, which happens where the change never acts. A
comparison takes one to two minutes with `--jobs 10`.

The map world is the calibrated world for the return trip. In recorded
Metroid campaigns, climbing out of Kraid's hideout after the kill takes
0.55–0.72 of the first trip from the hideout entry to Kraid's room. The map
rule asserts only that the return trip is shorter than the first trip.

## Scale measurements

`scale.py` runs scaled graph campaigns and prints measurements. It is not a CI
check, writes no files, and each mode runs for minutes to hours:

```sh
uv run workloads/tiny-worlds/scale.py slowdown --binary before --binary after
uv run workloads/tiny-worlds/scale.py cores --cost-ns 4000000 --work 6000 --repeats 3
uv run workloads/tiny-worlds/scale.py memory --workers 2 --work 1000000
```

Every run uses a 4,194,304-entry archive limit, two reservations per worker and
one result per worker unless `cores` sets them.
`slowdown` and `cores` use one runtime seed and one graph layout for all their
runs, so every build and worker count explores the same graph. A try on the
graph averages about 1.3 transitions, so `--cost-ns 4000000` gives about 5.2 ms
of worker CPU time per try.
The script samples the coordinator thread's CPU time, from `ps -M` on macOS
and from `/proc` schedstat on Linux, against the executions on the latest
progress line.

| Mode | Runs | Prints |
| --- | --- | --- |
| `slowdown` | One long campaign per `--binary` on a 4,194,304-node graph with 1,024 places, a 16 GiB budget, and no snapshot payload. | Executions per second and coordinator CPU milliseconds per 1,000 executions, as medians over sampling windows grouped into `--bin` active entries. The final window is dropped because it includes the final report. |
| `cores` | The same graph at each of `--workers-list`, with `--work` transitions per worker, or in total with `--total-work`, and with `--reservations` and `--results` per worker. | Median executions per second over `--repeats`, timed between the first and last progress lines of the search so setup and the final report are excluded, speedup over the first count, coordinator busy share, transitions per try, and worker milliseconds per try. |
| `memory` | `--seeds` campaigns on a 65,536-node graph under `--budget-mib`, plus one control at 16 GiB, with `--snapshot-bytes` payloads. | Whether each run reached the goal, peak logical memory, progress lines over the budget, peak RSS, snapshot evictions, history compactions, and dropped entries. |

## Checks

```sh
cargo test --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml -- --test-threads=1
cargo fmt --manifest-path workloads/tiny-worlds/Cargo.toml -- --check
cargo clippy --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml --all-targets -- -D warnings
```

Tests cover exhaustive reachability, dead trap items, snapshot restoration, archive retention in
both insertion orders, controlled objective witnesses, chain boundaries, route
alignment, trace validation, replay, and execution-work accounting.
